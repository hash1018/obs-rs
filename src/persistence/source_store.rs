use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{
    ColorSourceSettings, Crop, DisplayCaptureSettings, DisplayCaptureTarget, DrawingSourceSettings,
    SceneCanvas, SceneId, SceneItem, SceneItemId, Source, SourceId, SourceKind, SourceSettings,
    Stroke, Transform, WindowCaptureSettings, WindowCaptureTarget,
};

use super::PersistenceResult;

/// A stroke's points as they are stored: pairs of little-endian `f32`.
///
/// Little-endian by name rather than by host order, so a project file written
/// on one machine reads the same on another.
fn pack_points(points: &[[f32; 2]]) -> Vec<u8> {
    let mut packed = Vec::with_capacity(points.len() * 8);
    for [x, y] in points {
        packed.extend_from_slice(&x.to_le_bytes());
        packed.extend_from_slice(&y.to_le_bytes());
    }
    packed
}

/// The inverse. A trailing partial pair is dropped rather than refused: half a
/// point is not a point, and losing it costs one vertex of one stroke where
/// failing would cost the whole project.
fn unpack_points(packed: &[u8]) -> Vec<[f32; 2]> {
    packed
        .chunks_exact(8)
        .map(|pair| {
            let read = |bytes: &[u8]| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            [read(&pair[..4]), read(&pair[4..])]
        })
        .collect()
}

/// Which Source a SceneItem draws, which is where its settings live.
///
/// Every other Source command names an item too — an item is what the UI has
/// selected — so the resolution belongs here rather than in each caller.
fn source_of(transaction: &Transaction<'_>, item_id: SceneItemId) -> PersistenceResult<SourceId> {
    Ok(SourceId(transaction.query_row(
        "SELECT source_id FROM scene_items WHERE id = ?1",
        params![item_id.0],
        |row| row.get(0),
    )?))
}

pub(crate) struct SourceStore;

impl SourceStore {
    pub(crate) fn list_for_scene(
        connection: &Connection,
        scene_id: SceneId,
    ) -> PersistenceResult<Vec<(SceneItem, Source)>> {
        let mut statement = connection.prepare(
            "SELECT
                scene_items.id,
                scene_items.source_id,
                scene_items.visible,
                scene_items.locked,
                scene_items.position_x,
                scene_items.position_y,
                scene_items.scale_x,
                scene_items.scale_y,
                scene_items.rotation_degrees,
                scene_items.anchor_x,
                scene_items.anchor_y,
                scene_items.crop_left,
                scene_items.crop_top,
                scene_items.crop_right,
                scene_items.crop_bottom,
                scene_items.z_index,
                sources.name,
                sources.kind,
                color_source_settings.width,
                color_source_settings.height,
                color_source_settings.red,
                color_source_settings.green,
                color_source_settings.blue,
                color_source_settings.alpha,
                display_capture_settings.target_kind,
                display_capture_settings.monitor_name,
                display_capture_settings.restore_token,
                display_capture_settings.width,
                display_capture_settings.height,
                drawing_source_settings.width,
                drawing_source_settings.height,
                window_capture_settings.target_kind,
                window_capture_settings.process,
                window_capture_settings.title,
                window_capture_settings.restore_token,
                window_capture_settings.width,
                window_capture_settings.height
             FROM scene_items
             JOIN sources ON sources.id = scene_items.source_id
             LEFT JOIN color_source_settings
                ON color_source_settings.source_id = sources.id
             LEFT JOIN display_capture_settings
                ON display_capture_settings.source_id = sources.id
             LEFT JOIN drawing_source_settings
                ON drawing_source_settings.source_id = sources.id
             LEFT JOIN window_capture_settings
                ON window_capture_settings.source_id = sources.id
             WHERE scene_items.scene_id = ?1
             ORDER BY scene_items.z_index DESC, scene_items.id DESC",
        )?;

        let mut rows = statement
            .query_map([scene_id.0], |row| {
                let kind_name: String = row.get(17)?;
                let kind = SourceKind::from_storage_name(&kind_name).ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        17,
                        "kind".into(),
                        rusqlite::types::Type::Text,
                    )
                })?;
                let settings = match kind {
                    SourceKind::Color => SourceSettings::Color(ColorSourceSettings {
                        size: [row.get::<_, i64>(18)? as f32, row.get::<_, i64>(19)? as f32],
                        rgba: [
                            row.get::<_, i64>(20)? as u8,
                            row.get::<_, i64>(21)? as u8,
                            row.get::<_, i64>(22)? as u8,
                            row.get::<_, i64>(23)? as u8,
                        ],
                    }),
                    SourceKind::DisplayCapture => {
                        SourceSettings::DisplayCapture(DisplayCaptureSettings {
                            target: display_capture_target(
                                &row.get::<_, String>(24)?,
                                row.get(25)?,
                                row.get(26)?,
                            )
                            .ok_or_else(|| {
                                rusqlite::Error::InvalidColumnType(
                                    24,
                                    "target_kind".into(),
                                    rusqlite::types::Type::Text,
                                )
                            })?,
                            size_hint: match (row.get(27)?, row.get(28)?) {
                                (Some(width), Some(height)) => Some([width, height]),
                                _ => None,
                            },
                        })
                    }
                    // The strokes are not joined here — a Drawing has many and
                    // the rest of this query has one row per item. They are
                    // filled in below, once the items are known.
                    SourceKind::Drawing => SourceSettings::Drawing(DrawingSourceSettings {
                        size: [row.get::<_, i64>(29)? as f32, row.get::<_, i64>(30)? as f32],
                        strokes: Vec::new(),
                    }),
                    SourceKind::WindowCapture => {
                        SourceSettings::WindowCapture(WindowCaptureSettings {
                            target: window_capture_target(
                                &row.get::<_, String>(31)?,
                                row.get(32)?,
                                row.get(33)?,
                                row.get(34)?,
                            )
                            .ok_or_else(|| {
                                rusqlite::Error::InvalidColumnType(
                                    31,
                                    "target_kind".into(),
                                    rusqlite::types::Type::Text,
                                )
                            })?,
                            size_hint: match (row.get(35)?, row.get(36)?) {
                                (Some(width), Some(height)) => Some([width, height]),
                                _ => None,
                            },
                        })
                    }
                    _ => SourceSettings::None,
                };
                let source_id = SourceId(row.get(1)?);
                Ok((
                    SceneItem {
                        id: SceneItemId(row.get(0)?),
                        scene_id,
                        source_id,
                        visible: row.get(2)?,
                        locked: row.get(3)?,
                        transform: Transform {
                            position: [row.get(4)?, row.get(5)?],
                            scale: [row.get(6)?, row.get(7)?],
                            rotation_degrees: row.get(8)?,
                            anchor: [row.get(9)?, row.get(10)?],
                        },
                        crop: Crop {
                            left: row.get(11)?,
                            top: row.get(12)?,
                            right: row.get(13)?,
                            bottom: row.get(14)?,
                        },
                        z_index: row.get(15)?,
                    },
                    Source {
                        id: source_id,
                        name: row.get(16)?,
                        kind,
                        settings,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (_, source) in &mut rows {
            if let SourceSettings::Drawing(settings) = &mut source.settings {
                settings.strokes = Self::strokes(connection, source.id)?;
            }
        }
        Ok(rows)
    }

    /// Every stroke of one Drawing, in the order they were made.
    fn strokes(connection: &Connection, source_id: SourceId) -> PersistenceResult<Vec<Stroke>> {
        let mut statement = connection.prepare(
            "SELECT red, green, blue, alpha, width, points
               FROM drawing_strokes
              WHERE source_id = ?1
              ORDER BY ordinal",
        )?;
        Ok(statement
            .query_map([source_id.0], |row| {
                Ok(Stroke {
                    rgba: [
                        row.get::<_, i64>(0)? as u8,
                        row.get::<_, i64>(1)? as u8,
                        row.get::<_, i64>(2)? as u8,
                        row.get::<_, i64>(3)? as u8,
                    ],
                    width: row.get(4)?,
                    points: unpack_points(&row.get::<_, Vec<u8>>(5)?),
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    /// Every SceneItem the project holds, across all Scenes.
    pub(crate) fn live_item_ids(
        connection: &Connection,
    ) -> PersistenceResult<std::collections::HashSet<SceneItemId>> {
        let mut statement = connection.prepare("SELECT id FROM scene_items")?;
        Ok(statement
            .query_map([], |row| row.get::<_, i64>(0).map(SceneItemId))?
            .collect::<Result<_, _>>()?)
    }

    pub(crate) fn add_color(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Color Source")?;
        let source_id = create(transaction, &name, SourceKind::Color)?;
        let canvas = SceneCanvas::DEFAULT;
        let rgba = [53_u8, 91, 192, 255];
        transaction.execute(
            "INSERT INTO color_source_settings
                (source_id, width, height, red, green, blue, alpha)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                source_id.0,
                canvas.width as i64,
                canvas.height as i64,
                rgba[0],
                rgba[1],
                rgba[2],
                rgba[3]
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, canvas)
    }

    /// A new Drawing, the size of the Canvas and with nothing on it yet.
    pub(crate) fn add_drawing(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Drawing")?;
        let source_id = create(transaction, &name, SourceKind::Drawing)?;
        let canvas = SceneCanvas::DEFAULT;
        transaction.execute(
            "INSERT INTO drawing_source_settings (source_id, width, height)
             VALUES (?1, ?2, ?3)",
            params![source_id.0, canvas.width as i64, canvas.height as i64],
        )?;
        add_to_scene(transaction, scene_id, source_id, canvas)
    }

    /// Puts one stroke on the end of a Drawing.
    ///
    /// The ordinal is taken from what is already there rather than counted by
    /// the caller, so an undo followed by a new stroke reuses the number the
    /// undone one gave up instead of leaving a hole.
    pub(crate) fn add_stroke(
        transaction: &Transaction<'_>,
        item_id: SceneItemId,
        stroke: &Stroke,
    ) -> PersistenceResult<()> {
        let source_id = source_of(transaction, item_id)?;
        transaction.execute(
            "INSERT INTO drawing_strokes
                (source_id, ordinal, red, green, blue, alpha, width, points)
             VALUES (
                ?1,
                (SELECT COALESCE(MAX(ordinal) + 1, 0)
                   FROM drawing_strokes WHERE source_id = ?1),
                ?2, ?3, ?4, ?5, ?6, ?7
             )",
            params![
                source_id.0,
                stroke.rgba[0],
                stroke.rgba[1],
                stroke.rgba[2],
                stroke.rgba[3],
                stroke.width,
                pack_points(&stroke.points),
            ],
        )?;
        Ok(())
    }

    /// Takes strokes off a Drawing by their position in it, which is what the
    /// eraser and undo both do.
    ///
    /// Removing by ordinal rather than rewriting the list keeps a stroke's
    /// identity stable while a gesture is in progress; the ordinals left
    /// behind stay ordered, which is all anything reads them for.
    pub(crate) fn remove_strokes(
        transaction: &Transaction<'_>,
        item_id: SceneItemId,
        ordinals: &[usize],
    ) -> PersistenceResult<()> {
        let source_id = source_of(transaction, item_id)?;
        let mut statement = transaction.prepare(
            "DELETE FROM drawing_strokes
              WHERE source_id = ?1
                AND ordinal = (
                    SELECT ordinal FROM drawing_strokes
                     WHERE source_id = ?1 ORDER BY ordinal LIMIT 1 OFFSET ?2
                )",
        )?;
        // Highest first, so each offset still names the stroke it did when the
        // caller worked them out.
        let mut ordinals = ordinals.to_vec();
        ordinals.sort_unstable();
        for ordinal in ordinals.into_iter().rev() {
            statement.execute(params![source_id.0, ordinal as i64])?;
        }
        Ok(())
    }

    /// Everything drawn on one Drawing, gone.
    pub(crate) fn clear_strokes(
        transaction: &Transaction<'_>,
        item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        let source_id = source_of(transaction, item_id)?;
        transaction.execute(
            "DELETE FROM drawing_strokes WHERE source_id = ?1",
            params![source_id.0],
        )?;
        Ok(())
    }

    pub(crate) fn add_display_capture(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        settings: &DisplayCaptureSettings,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Display Capture")?;
        let source_id = create(transaction, &name, SourceKind::DisplayCapture)?;
        let (kind, monitor_name, restore_token) = match &settings.target {
            DisplayCaptureTarget::MonitorName(monitor_name) => {
                ("monitor", Some(monitor_name.as_str()), None)
            }
            DisplayCaptureTarget::Portal { restore_token } => {
                ("portal", None, restore_token.as_deref())
            }
        };
        let [width, height] = settings
            .size_hint
            .map_or([None, None], |[width, height]| [Some(width), Some(height)]);
        transaction.execute(
            "INSERT INTO display_capture_settings
                (source_id, target_kind, monitor_name, restore_token, width, height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source_id.0,
                kind,
                monitor_name,
                restore_token,
                width,
                height
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, SceneCanvas::DEFAULT)
    }

    pub(crate) fn add_window_capture(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        settings: &WindowCaptureSettings,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Window Capture")?;
        let source_id = create(transaction, &name, SourceKind::WindowCapture)?;
        let (kind, process, title, restore_token) = match &settings.target {
            WindowCaptureTarget::Window { process, title } => {
                ("window", Some(process.as_str()), Some(title.as_str()), None)
            }
            WindowCaptureTarget::Portal { restore_token } => {
                ("portal", None, None, restore_token.as_deref())
            }
        };
        let [width, height] = settings
            .size_hint
            .map_or([None, None], |[width, height]| [Some(width), Some(height)]);
        transaction.execute(
            "INSERT INTO window_capture_settings
                (source_id, target_kind, process, title, restore_token, width, height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                source_id.0,
                kind,
                process,
                title,
                restore_token,
                width,
                height
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, SceneCanvas::DEFAULT)
    }

    pub(crate) fn set_transform(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        transform: Transform,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items
             SET position_x = ?1,
                 position_y = ?2,
                 scale_x = ?3,
                 scale_y = ?4,
                 rotation_degrees = ?5,
                 anchor_x = ?6,
                 anchor_y = ?7
             WHERE id = ?8",
            params![
                transform.position[0],
                transform.position[1],
                transform.scale[0],
                transform.scale[1],
                transform.rotation_degrees,
                transform.anchor[0],
                transform.anchor[1],
                scene_item_id.0
            ],
        )?;
        Ok(())
    }

    /// Records the token a portal handed back when the Source was opened.
    ///
    /// Scoped to portal targets: a monitor name is resolved against the live
    /// display layout and has no token, so writing one there would be a row
    /// the schema's own CHECK constraint rejects.
    pub(crate) fn set_restore_token(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        restore_token: Option<&str>,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE display_capture_settings
                SET restore_token = ?1
              WHERE target_kind = 'portal'
                AND source_id = (
                    SELECT source_id FROM scene_items WHERE id = ?2
                )",
            params![restore_token, scene_item_id.0],
        )?;
        Ok(())
    }

    /// Repaints a Color Source.
    ///
    /// Keyed by the SceneItem the caller has in hand rather than by the
    /// Source, the same way every other command here is: which Source an item
    /// stands for is this store's to resolve, not each caller's. Two items on
    /// one Color Source therefore change together, which is what sharing a
    /// Source means.
    pub(crate) fn set_color(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        rgba: [u8; 4],
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE color_source_settings
                SET red = ?2, green = ?3, blue = ?4, alpha = ?5
              WHERE source_id = (
                  SELECT source_id FROM scene_items WHERE id = ?1
              )",
            params![scene_item_id.0, rgba[0], rgba[1], rgba[2], rgba[3]],
        )?;
        Ok(())
    }

    pub(crate) fn set_visible(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        visible: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items SET visible = ?1 WHERE id = ?2",
            params![visible, scene_item_id.0],
        )?;
        Ok(())
    }

    pub(crate) fn set_locked(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        locked: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items SET locked = ?1 WHERE id = ?2",
            params![locked, scene_item_id.0],
        )?;
        Ok(())
    }

    /// Moves an item in front of the one currently ahead of it.
    pub(crate) fn move_up(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        swap_with_neighbour(transaction, scene_item_id, Neighbour::Above)
    }

    /// Moves an item behind the one currently after it.
    pub(crate) fn move_down(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        swap_with_neighbour(transaction, scene_item_id, Neighbour::Below)
    }

    pub(crate) fn delete_scene_item(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        let source_id = transaction.query_row(
            "SELECT source_id FROM scene_items WHERE id = ?1",
            [scene_item_id.0],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute("DELETE FROM scene_items WHERE id = ?1", [scene_item_id.0])?;
        transaction.execute(
            "DELETE FROM sources
             WHERE id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM scene_items WHERE source_id = ?1
               )",
            [source_id],
        )?;
        Ok(())
    }
}

/// Rebuilds the target a Display Capture row stores. The schema's own CHECK
/// constraint already rejects the combinations this cannot map, so `None` here
/// means a row written by something other than [`SourceStore`].
fn display_capture_target(
    kind: &str,
    monitor_name: Option<String>,
    restore_token: Option<String>,
) -> Option<DisplayCaptureTarget> {
    match kind {
        "monitor" => monitor_name.map(DisplayCaptureTarget::MonitorName),
        "portal" => Some(DisplayCaptureTarget::Portal { restore_token }),
        _ => None,
    }
}

/// The stored pair back into a target, or `None` for a row the schema's own
/// CHECK should already have refused.
fn window_capture_target(
    kind: &str,
    process: Option<String>,
    title: Option<String>,
    restore_token: Option<String>,
) -> Option<WindowCaptureTarget> {
    match kind {
        "window" => Some(WindowCaptureTarget::Window {
            process: process?,
            title: title?,
        }),
        "portal" => Some(WindowCaptureTarget::Portal { restore_token }),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Neighbour {
    /// Composited in front, which is a higher `z_index`.
    Above,
    Below,
}

/// Exchanges an item's compositing order with its adjacent neighbour.
///
/// The neighbour is found by ordering rather than by an expected `z_index`
/// value: deleting an item leaves a gap, so the two are not adjacent numbers
/// for long, and looking one up by `z_index + 1` would silently do nothing.
fn swap_with_neighbour(
    transaction: &Transaction<'_>,
    scene_item_id: SceneItemId,
    neighbour: Neighbour,
) -> PersistenceResult<()> {
    let Some((scene_id, z_index)) = transaction
        .query_row(
            "SELECT scene_id, z_index FROM scene_items WHERE id = ?1",
            [scene_item_id.0],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    else {
        return Ok(());
    };

    let query = match neighbour {
        Neighbour::Above => {
            "SELECT id, z_index FROM scene_items
             WHERE scene_id = ?1 AND z_index > ?2
             ORDER BY z_index ASC LIMIT 1"
        }
        Neighbour::Below => {
            "SELECT id, z_index FROM scene_items
             WHERE scene_id = ?1 AND z_index < ?2
             ORDER BY z_index DESC LIMIT 1"
        }
    };
    let Some((other_id, other_z)) = transaction
        .query_row(query, params![scene_id, z_index], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?
    else {
        return Ok(());
    };

    transaction.execute(
        "UPDATE scene_items
         SET z_index = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END
         WHERE id IN (?1, ?3)",
        params![scene_item_id.0, other_z, other_id, z_index],
    )?;
    Ok(())
}

fn create(
    transaction: &Transaction<'_>,
    name: &str,
    kind: SourceKind,
) -> PersistenceResult<SourceId> {
    transaction.execute(
        "INSERT INTO sources (name, kind) VALUES (?1, ?2)",
        params![name, kind.storage_name()],
    )?;
    Ok(SourceId(transaction.last_insert_rowid()))
}

fn add_to_scene(
    transaction: &Transaction<'_>,
    scene_id: SceneId,
    source_id: SourceId,
    canvas: SceneCanvas,
) -> PersistenceResult<SceneItemId> {
    transaction.execute(
        "INSERT INTO scene_items
                (scene_id, source_id, position_x, position_y, z_index)
             VALUES (
                ?1,
                ?2,
                ?3,
                ?4,
                COALESCE(
                    (SELECT MAX(z_index) + 1 FROM scene_items WHERE scene_id = ?1),
                    0
                )
             )",
        params![
            scene_id.0,
            source_id.0,
            canvas.width * 0.5,
            canvas.height * 0.5
        ],
    )?;
    Ok(SceneItemId(transaction.last_insert_rowid()))
}

fn unique_source_name(connection: &Connection, base: &str) -> PersistenceResult<String> {
    let mut suffix = 1;
    loop {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base} {suffix}")
        };
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE name = ?1)",
            [&candidate],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        suffix += 1;
    }
}
