//! Edit → Undo and Redo, put into words.
//!
//! Shared by the menu, which names the step it would move, and the status
//! bar, which names the one it just moved — the same sentence in both, so
//! what the menu promised is what the status bar reports.

use crate::i18n::{LocalizationManager, TextKey};
use crate::snapshots::{EditLabel, EditVerb};

/// One step as a sentence: "Move Scene 1 › Webcam", "씬 1 › Webcam 이동·크기
/// 변경". Word order is each language's own, which is why every verb is a
/// whole message with the target placed inside it rather than a word put in
/// front of one.
pub(super) fn describe(label: &EditLabel, i18n: &LocalizationManager) -> String {
    let key = match label.verb {
        EditVerb::AddScene => TextKey::EditAddScene,
        EditVerb::DeleteScene => TextKey::EditDeleteScene,
        EditVerb::DuplicateScene => TextKey::EditDuplicateScene,
        EditVerb::AddSource => TextKey::EditAddSource,
        EditVerb::Delete => TextKey::EditDelete,
        EditVerb::Rename => TextKey::EditRename,
        EditVerb::Reorder => TextKey::EditReorder,
        EditVerb::Transform => TextKey::EditTransform,
        EditVerb::Crop => TextKey::EditCrop,
        EditVerb::Opacity => TextKey::EditOpacity,
        EditVerb::Visibility => TextKey::EditVisibility,
        EditVerb::Fade => TextKey::EditFade,
        EditVerb::Lock => TextKey::EditLock,
        EditVerb::Properties => TextKey::EditProperties,
        EditVerb::AddFilter => TextKey::EditAddFilter,
        EditVerb::RemoveFilter => TextKey::EditRemoveFilter,
        EditVerb::ReorderFilter => TextKey::EditReorderFilter,
        EditVerb::FilterSettings => TextKey::EditFilterSettings,
        EditVerb::Draw => TextKey::EditDraw,
        EditVerb::Erase => TextKey::EditErase,
        EditVerb::Transition => TextKey::EditTransition,
        EditVerb::AddChannel => TextKey::EditAddChannel,
        EditVerb::RemoveChannel => TextKey::EditRemoveChannel,
        EditVerb::ChannelDevice => TextKey::EditChannelDevice,
    };
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("target", label.target.clone());
    i18n.text_with(key, &args).into_owned()
}

/// "Undo Move Scene 1 › Webcam", or plain "Undo" where there is nothing to.
pub(super) fn menu_item(
    label: Option<&EditLabel>,
    with: TextKey,
    without: TextKey,
    i18n: &LocalizationManager,
) -> String {
    match label {
        Some(label) => {
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("action", describe(label, i18n));
            i18n.text_with(with, &args).into_owned()
        }
        None => i18n.text(without).into_owned(),
    }
}
