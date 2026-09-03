use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{
    ColorSourceSettings, Crop, DisplayCaptureSettings, DisplayCaptureTarget, DrawingSourceSettings,
    ImageSourceSettings, MAX_GAIN_DB, MIN_GAIN_DB, MediaFileSettings, RtspSourceSettings,
    RtspTransport, SceneCanvas, SceneId, SceneItem, SceneItemId, Source, SourceId, SourceKind,
    SourceSettings, Stroke, Transform, WindowCaptureSettings, WindowCaptureTarget,
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
        .as_chunks::<8>()
        .0
        .iter()
        .map(|pair| {
            let (halves, _) = pair.as_chunks::<4>();
            [f32::from_le_bytes(halves[0]), f32::from_le_bytes(halves[1])]
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
                window_capture_settings.height,
                media_file_settings.path,
                media_file_settings.looping,
                media_file_settings.width,
                media_file_settings.height,
                media_file_settings.has_audio,
                media_file_settings.gain_db,
                media_file_settings.muted,
                media_file_settings.duration_us,
                media_file_settings.paused,
                media_file_settings.monitored,
                image_source_settings.path,
                image_source_settings.width,
                image_source_settings.height,
                rtsp_source_settings.url,
                rtsp_source_settings.transport,
                rtsp_source_settings.reconnect_seconds,
                rtsp_source_settings.width,
                rtsp_source_settings.height,
                rtsp_source_settings.has_audio,
                rtsp_source_settings.gain_db,
                rtsp_source_settings.muted
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
             LEFT JOIN media_file_settings
                ON media_file_settings.source_id = sources.id
             LEFT JOIN image_source_settings
                ON image_source_settings.source_id = sources.id
             LEFT JOIN rtsp_source_settings
                ON rtsp_source_settings.source_id = sources.id
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
                    SourceKind::MediaFile => SourceSettings::MediaFile(MediaFileSettings {
                        path: PathBuf::from(row.get::<_, String>(37)?),
                        looping: row.get(38)?,
                        size_hint: match (row.get(39)?, row.get(40)?) {
                            (Some(width), Some(height)) => Some([width, height]),
                            _ => None,
                        },
                        has_audio: row.get(41)?,
                        gain_db: row.get(42)?,
                        muted: row.get(43)?,
                        duration: row
                            .get::<_, Option<i64>>(44)?
                            .and_then(|micros| u64::try_from(micros).ok())
                            .map(std::time::Duration::from_micros),
                        paused: row.get(45)?,
                        monitored: row.get(46)?,
                    }),
                    SourceKind::Image => SourceSettings::Image(ImageSourceSettings {
                        path: PathBuf::from(row.get::<_, String>(47)?),
                        size_hint: match (row.get(48)?, row.get(49)?) {
                            (Some(width), Some(height)) => Some([width, height]),
                            _ => None,
                        },
                    }),
                    SourceKind::Rtsp => SourceSettings::Rtsp(RtspSourceSettings {
                        url: row.get(50)?,
                        transport: RtspTransport::from_storage_name(&row.get::<_, String>(51)?)
                            .ok_or_else(|| {
                                rusqlite::Error::InvalidColumnType(
                                    51,
                                    "transport".into(),
                                    rusqlite::types::Type::Text,
                                )
                            })?,
                        reconnect: row
                            .get::<_, Option<i64>>(52)?
                            .and_then(|seconds| u64::try_from(seconds).ok())
                            .map(std::time::Duration::from_secs),
                        size_hint: match (row.get(53)?, row.get(54)?) {
                            (Some(width), Some(height)) => Some([width, height]),
                            _ => None,
                        },
                        has_audio: row.get(55)?,
                        gain_db: row.get(56)?,
                        muted: row.get(57)?,
                    }),
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
    ) -> PersistenceResult<HashSet<SceneItemId>> {
        let mut statement = connection.prepare("SELECT id FROM scene_items")?;
        Ok(statement
            .query_map([], |row| row.get::<_, i64>(0).map(SceneItemId))?
            .collect::<Result<_, _>>()?)
    }

    /// Every Source's name in the project.
    ///
    /// `sources.name` is UNIQUE, so this is what a caller checks a name
    /// against before offering it: a collision is a refused transaction, not
    /// an edit, and one Scene's items are not all the names there are.
    pub(crate) fn names(connection: &Connection) -> PersistenceResult<HashSet<String>> {
        let mut statement = connection.prepare("SELECT name FROM sources")?;
        Ok(statement
            .query_map([], |row| row.get::<_, String>(0))?
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

    /// Adds a media file Source, named after the file itself.
    ///
    /// The name is the file's own stem because that is what someone reading
    /// the Sources list is looking for, and because the alternative — "Media
    /// Source 4" — names nothing. It is still only a starting name: it is
    /// stored, not derived, so renaming it sticks and moving the file does
    /// not rename anything.
    pub(crate) fn add_media_file(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        settings: &MediaFileSettings,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, &file_stem(&settings.path, "Media Source"))?;
        let source_id = create(transaction, &name, SourceKind::MediaFile)?;
        let [width, height] = settings
            .size_hint
            .map_or([None, None], |[width, height]| [Some(width), Some(height)]);
        transaction.execute(
            "INSERT INTO media_file_settings
                (source_id, path, looping, width, height, has_audio, gain_db, muted,
                 duration_us, paused)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                source_id.0,
                settings.path.to_string_lossy(),
                settings.looping,
                width,
                height,
                settings.has_audio,
                settings.gain_db,
                settings.muted,
                settings
                    .duration
                    .and_then(|duration| i64::try_from(duration.as_micros()).ok()),
                settings.paused
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, SceneCanvas::DEFAULT)
    }

    /// Adds a live stream Source, named after the address it pulls from.
    ///
    /// The last path segment rather than the whole URL: `rtsp://10.0.0.7/live`
    /// becomes "live", which is short enough for the dock and is what
    /// distinguishes two streams off one camera. A URL with nothing to take
    /// falls back to the host, and one with neither to the kind's own name —
    /// the same ladder [`SourceStore::add_media_file`] climbs.
    pub(crate) fn add_rtsp(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        settings: &RtspSourceSettings,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, &stream_name(&settings.url))?;
        let source_id = create(transaction, &name, SourceKind::Rtsp)?;
        let [width, height] = settings
            .size_hint
            .map_or([None, None], |[width, height]| [Some(width), Some(height)]);
        transaction.execute(
            "INSERT INTO rtsp_source_settings
                (source_id, url, transport, reconnect_seconds, width, height,
                 has_audio, gain_db, muted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                source_id.0,
                settings.url,
                settings.transport.storage_name(),
                settings
                    .reconnect
                    .and_then(|wait| i64::try_from(wait.as_secs()).ok())
                    .filter(|seconds| *seconds > 0),
                width,
                height,
                settings.has_audio,
                settings.gain_db,
                settings.muted
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, SceneCanvas::DEFAULT)
    }

    /// How this stream's session carries its video. Takes effect by reopening
    /// the Source — a transport is negotiated at connect and there is nothing
    /// to change about a session that is already running.
    pub(crate) fn set_rtsp_transport(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        transport: RtspTransport,
    ) -> PersistenceResult<()> {
        set_rtsp_column(
            transaction,
            scene_item_id,
            "transport",
            transport.storage_name(),
        )
    }

    /// How long to wait before connecting again, or `None` to wait to be
    /// asked. Stored as absence rather than as zero — see the migration.
    pub(crate) fn set_rtsp_reconnect(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        reconnect: Option<std::time::Duration>,
    ) -> PersistenceResult<()> {
        set_rtsp_column(
            transaction,
            scene_item_id,
            "reconnect_seconds",
            reconnect
                .and_then(|wait| i64::try_from(wait.as_secs()).ok())
                .filter(|seconds| *seconds > 0),
        )
    }

    /// Adds an image Source, named after the file — see
    /// [`SourceStore::add_media_file`] for why the file's own name.
    pub(crate) fn add_image(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        settings: &ImageSourceSettings,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, &file_stem(&settings.path, "Image"))?;
        let source_id = create(transaction, &name, SourceKind::Image)?;
        let [width, height] = settings
            .size_hint
            .map_or([None, None], |[width, height]| [Some(width), Some(height)]);
        transaction.execute(
            "INSERT INTO image_source_settings (source_id, path, width, height)
             VALUES (?1, ?2, ?3, ?4)",
            params![source_id.0, settings.path.to_string_lossy(), width, height],
        )?;
        add_to_scene(transaction, scene_id, source_id, SceneCanvas::DEFAULT)
    }

    /// Whether this media file Source starts again at its end.
    pub(crate) fn set_media_looping(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        looping: bool,
    ) -> PersistenceResult<()> {
        set_media_column(transaction, scene_item_id, "looping", looping)
    }

    /// This media file Source's own fader, in decibels.
    pub(crate) fn set_media_gain_db(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        gain_db: f32,
    ) -> PersistenceResult<()> {
        set_media_column(
            transaction,
            scene_item_id,
            "gain_db",
            gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB),
        )
    }

    /// Whether this media file Source is stopped where it is. Hiding the
    /// SceneItem stops it too, and does not come through here — see
    /// [`crate::domain::MediaFileSettings::paused`].
    pub(crate) fn set_media_paused(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        paused: bool,
    ) -> PersistenceResult<()> {
        set_media_column(transaction, scene_item_id, "paused", paused)
    }

    /// This media file Source's own mute button. Hiding the SceneItem
    /// silences it too, and does not come through here — see
    /// [`crate::domain::MediaFileSettings::muted`].
    pub(crate) fn set_media_muted(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        muted: bool,
    ) -> PersistenceResult<()> {
        set_media_column(transaction, scene_item_id, "muted", muted)
    }

    pub(crate) fn set_media_monitored(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        monitored: bool,
    ) -> PersistenceResult<()> {
        set_media_column(transaction, scene_item_id, "monitored", monitored)
    }

    /// How much of the Source this item leaves out, in the Source's own
    /// pixels.
    ///
    /// On the item rather than the Source, unlike a colour or a file path:
    /// two items standing for one capture crop it differently, which is the
    /// whole point of cropping one of them.
    pub(crate) fn set_crop(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        crop: Crop,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items
             SET crop_left = ?1,
                 crop_top = ?2,
                 crop_right = ?3,
                 crop_bottom = ?4
             WHERE id = ?5",
            params![
                crop.left.max(0.0),
                crop.top.max(0.0),
                crop.right.max(0.0),
                crop.bottom.max(0.0),
                scene_item_id.0
            ],
        )?;
        Ok(())
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
        // Both tables, because both kinds of capture are opened through the
        // portal on the platforms that have one, and only one of them holds a
        // row for any given Source.
        transaction.execute(
            "UPDATE display_capture_settings
                SET restore_token = ?1
              WHERE target_kind = 'portal'
                AND source_id = (
                    SELECT source_id FROM scene_items WHERE id = ?2
                )",
            params![restore_token, scene_item_id.0],
        )?;
        transaction.execute(
            "UPDATE window_capture_settings
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

    /// Renames a Source.
    ///
    /// Keyed by the SceneItem in hand, the way [`SourceStore::set_color`] is:
    /// the name is the Source's, so every item standing for that Source shows
    /// the new one at once.
    ///
    /// The name is taken as given. `sources.name` is UNIQUE and this does not
    /// make a colliding name unique for the caller — a person who typed a name
    /// that is taken wants to hear so, not to be given a different one.
    pub(crate) fn rename(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        name: &str,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE sources
                SET name = ?2
              WHERE id = (SELECT source_id FROM scene_items WHERE id = ?1)",
            params![scene_item_id.0, name],
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

/// One column of the media file settings behind a SceneItem's Source.
///
/// The column name is a literal from the three callers above and never comes
/// from outside, which is what makes formatting it into the statement safe —
/// a bound parameter cannot name a column.
fn set_media_column<T: rusqlite::ToSql>(
    transaction: &Transaction<'_>,
    scene_item_id: SceneItemId,
    column: &'static str,
    value: T,
) -> PersistenceResult<()> {
    transaction.execute(
        &format!(
            "UPDATE media_file_settings
             SET {column} = ?1
             WHERE source_id = (SELECT source_id FROM scene_items WHERE id = ?2)"
        ),
        params![value, scene_item_id.0],
    )?;
    Ok(())
}

fn set_rtsp_column<T: rusqlite::ToSql>(
    transaction: &Transaction<'_>,
    scene_item_id: SceneItemId,
    column: &'static str,
    value: T,
) -> PersistenceResult<()> {
    transaction.execute(
        &format!(
            "UPDATE rtsp_source_settings
             SET {column} = ?1
             WHERE source_id = (SELECT source_id FROM scene_items WHERE id = ?2)"
        ),
        params![value, scene_item_id.0],
    )?;
    Ok(())
}

/// What to call a Source made from a URL.
///
/// The last non-empty path segment, then the host, then the kind's own name.
/// A whole URL would fill the dock and elide to its scheme, which is the half
/// that says nothing about which stream this is.
fn stream_name(url: &str) -> String {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (host, path) = without_scheme
        .split_once('/')
        .map_or((without_scheme, ""), |(host, path)| (host, path));
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .or(Some(host).filter(|host| !host.is_empty()))
        .map_or_else(|| "Network Stream".to_owned(), |name| name.to_owned())
}

/// What to call a Source made from a file: the file's own name, or `fallback`
/// for a path that has none to give.
fn file_stem(path: &std::path::Path, fallback: &str) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{ProjectDatabase, SceneStore};

    fn scene(database: &ProjectDatabase) -> SceneId {
        SceneStore::selected_scene_id(database.connection())
            .unwrap()
            .expect("a new project opens with a Scene selected")
    }

    fn settings_of(database: &ProjectDatabase, scene_id: SceneId) -> SourceSettings {
        SourceStore::list_for_scene(database.connection(), scene_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("the Scene has an item")
            .1
            .settings
    }

    fn names_in_scene(database: &ProjectDatabase, scene_id: SceneId) -> Vec<String> {
        SourceStore::list_for_scene(database.connection(), scene_id)
            .unwrap()
            .into_iter()
            .map(|(_, source)| source.name)
            .collect()
    }

    #[test]
    fn a_renamed_source_keeps_the_name_it_was_given() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene(&database);
        let item_id = database
            .transaction(|transaction| SourceStore::add_color(transaction, scene_id))
            .unwrap();

        database
            .transaction(|transaction| SourceStore::rename(transaction, item_id, "Backdrop"))
            .unwrap();

        assert_eq!(names_in_scene(&database, scene_id), vec!["Backdrop"]);
    }

    /// The dock offers the name it reads, and what it reads is
    /// [`SourceStore::names`]. A name missing from that set is one the dock
    /// would offer and the database would then refuse.
    #[test]
    fn every_source_name_is_offered_for_checking_against() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene(&database);
        database
            .transaction(|transaction| {
                SourceStore::add_color(transaction, scene_id)?;
                SourceStore::add_drawing(transaction, scene_id)
            })
            .unwrap();

        let names = SourceStore::names(database.connection()).unwrap();

        assert_eq!(
            names,
            HashSet::from(["Color Source".to_owned(), "Drawing".to_owned()])
        );
    }

    /// A media file's own settings, out of the read that follows the write.
    ///
    /// The monitor mode is the newest column and the reason this exists: the
    /// read takes its values by position, so a column added in the middle
    /// shifts every one after it. A file that came back with a stream's URL
    /// in its path would be caught by the assertions here long before
    /// anybody saw it.
    #[test]
    fn a_media_file_keeps_its_settings_including_how_it_is_monitored() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene(&database);
        let item_id = database
            .transaction(|transaction| {
                SourceStore::add_media_file(
                    transaction,
                    scene_id,
                    &MediaFileSettings {
                        path: PathBuf::from("/tmp/clip.mp4"),
                        looping: true,
                        size_hint: Some([1280, 720]),
                        has_audio: true,
                        gain_db: -3.0,
                        duration: Some(std::time::Duration::from_secs(42)),
                        paused: false,
                        muted: false,
                        monitored: false,
                    },
                )
            })
            .unwrap();

        let SourceSettings::MediaFile(stored) = settings_of(&database, scene_id) else {
            panic!("a media file Source must read back as one");
        };
        assert_eq!(stored.path, PathBuf::from("/tmp/clip.mp4"));
        assert!(stored.looping);
        assert_eq!(stored.size_hint, Some([1280, 720]));
        assert!(stored.has_audio);
        assert_eq!(stored.gain_db, -3.0);
        assert_eq!(stored.duration, Some(std::time::Duration::from_secs(42)));
        assert!(!stored.monitored);

        database
            .transaction(|transaction| SourceStore::set_media_monitored(transaction, item_id, true))
            .unwrap();

        let SourceSettings::MediaFile(stored) = settings_of(&database, scene_id) else {
            panic!("a media file Source must read back as one");
        };
        assert!(stored.monitored);
        // ...and the rest of the row is where it was, which is the half a
        // shifted column would break silently.
        assert_eq!(stored.path, PathBuf::from("/tmp/clip.mp4"));
        assert!(stored.has_audio);
    }

    /// The three things a stream stores that nothing else does, through the
    /// write and back out of the read — a transport that came back as the
    /// other one, or a reconnect that came back as zero, would each be a
    /// Source behaving differently after a restart than before it.
    #[test]
    fn a_stream_keeps_its_address_transport_and_reconnect() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene(&database);
        let item_id = database
            .transaction(|transaction| {
                SourceStore::add_rtsp(
                    transaction,
                    scene_id,
                    &RtspSourceSettings {
                        url: "rtsp://10.0.0.7:554/main".to_owned(),
                        transport: RtspTransport::Udp,
                        reconnect: Some(std::time::Duration::from_secs(15)),
                        size_hint: Some([1920, 1080]),
                        has_audio: true,
                        gain_db: 0.0,
                        muted: false,
                    },
                )
            })
            .unwrap();

        let SourceSettings::Rtsp(stored) = settings_of(&database, scene_id) else {
            panic!("a stream Source must read back as one");
        };
        assert_eq!(stored.url, "rtsp://10.0.0.7:554/main");
        assert_eq!(stored.transport, RtspTransport::Udp);
        assert_eq!(stored.reconnect, Some(std::time::Duration::from_secs(15)));
        assert_eq!(stored.size_hint, Some([1920, 1080]));
        assert!(stored.has_audio);

        database
            .transaction(|transaction| {
                SourceStore::set_rtsp_transport(transaction, item_id, RtspTransport::Tcp)?;
                SourceStore::set_rtsp_reconnect(transaction, item_id, None)
            })
            .unwrap();

        let SourceSettings::Rtsp(changed) = settings_of(&database, scene_id) else {
            panic!("a stream Source must read back as one");
        };
        assert_eq!(changed.transport, RtspTransport::Tcp);
        assert_eq!(
            changed.reconnect, None,
            "reconnecting never is stored as absence rather than as zero"
        );
    }

    /// A dock is narrow and a URL is not. What tells two streams off one
    /// camera apart is the end of the path, which is why that is what a new
    /// Source is called.
    #[test]
    fn a_stream_is_named_after_the_end_of_its_path() {
        assert_eq!(stream_name("rtsp://10.0.0.7:554/main"), "main");
        assert_eq!(stream_name("rtsp://10.0.0.7:554/cam/1/sub/"), "sub");
        assert_eq!(
            stream_name("rtsp://10.0.0.7:554"),
            "10.0.0.7:554",
            "an address with no path is named after the camera"
        );
        assert_eq!(stream_name(""), "Network Stream");
    }

    /// Why the dock checks before it sends: a taken name is not an edit that
    /// silently does nothing, it is a transaction that fails.
    #[test]
    fn a_name_another_source_holds_is_refused() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene(&database);
        let item_id = database
            .transaction(|transaction| {
                SourceStore::add_drawing(transaction, scene_id)?;
                SourceStore::add_color(transaction, scene_id)
            })
            .unwrap();

        let result = database
            .transaction(|transaction| SourceStore::rename(transaction, item_id, "Drawing"));

        assert!(result.is_err(), "two Sources cannot share one name");
        // And the refused transaction leaves the Source called what it was.
        assert_eq!(
            SourceStore::names(database.connection()).unwrap(),
            HashSet::from(["Color Source".to_owned(), "Drawing".to_owned()])
        );
    }

    /// A Source is the project's, not one Scene's: renaming it through any
    /// item that stands for it renames it in every Scene it appears in.
    #[test]
    fn renaming_through_one_item_renames_the_source_in_every_scene() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let original = scene(&database);
        database
            .transaction(|transaction| SourceStore::add_color(transaction, original))
            .unwrap();
        database
            .transaction(|transaction| SceneStore::duplicate(transaction, original))
            .unwrap();
        let copy = scene(&database);
        assert_ne!(copy, original, "duplicating selects the copy");

        let copied_item = SourceStore::list_for_scene(database.connection(), copy).unwrap()[0]
            .0
            .id;
        database
            .transaction(|transaction| SourceStore::rename(transaction, copied_item, "Backdrop"))
            .unwrap();

        assert_eq!(names_in_scene(&database, original), vec!["Backdrop"]);
        assert_eq!(names_in_scene(&database, copy), vec!["Backdrop"]);
    }
}
