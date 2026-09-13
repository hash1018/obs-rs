//! Every string the interface can show, named once.
//!
//! # Why a macro
//!
//! A key is three things: an enum variant, the identifier the language packs
//! use, and membership of the list [`TextKey::ALL`] that
//! `every_key_is_translated_in_every_locale` walks. Written out by hand those
//! are three lists of the same two hundred and fifty items, kept in step by
//! whoever remembers to. The macro makes them one list — a key that is
//! declared is a key that has an identifier and is in `ALL`, and there is no
//! way to add one that is not.
//!
//! What that buys is the test at the bottom of `manager`: a key with no
//! translation shows its own identifier in the interface rather than failing,
//! so nothing but a walk of every key finds a missing one.

macro_rules! text_keys {
    ($($variant:ident => $id:literal;)*) => {
        /// Every string the interface can show.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum TextKey {
            $($variant,)*
        }

        impl TextKey {
            /// Every key there is, for whoever has to check them all.
            ///
            /// Only the tests walk it today, and this crate is built with
            /// `-D warnings`, so outside them it is deliberately unused
            /// rather than absent: what it exists for is to be complete, and
            /// a list that is only built when someone remembers to would not
            /// be.
            #[cfg_attr(not(test), allow(dead_code))]
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// What the language packs call this one.
            pub const fn id(self) -> &'static str {
                match self {
                    $(Self::$variant => $id,)*
                }
            }
        }
    };
}

text_keys! {
    MenuFile                      => "menu-file";
    MenuExit                      => "menu-exit";
    MenuView                      => "menu-view";
    MenuFullscreen                => "menu-fullscreen";
    MenuDocks                     => "menu-docks";
    MenuTheme                     => "menu-theme";
    ThemeSystem                   => "theme-system";
    ThemeLight                    => "theme-light";
    ThemeDark                     => "theme-dark";
    MenuLanguage                  => "menu-language";
    LanguageEnglish               => "language-english";
    LanguageKorean                => "language-korean";
    MenuHelp                      => "menu-help";
    MenuAbout                     => "menu-about";
    AboutDescription              => "about-description";
    DockScenes                    => "dock-scenes";
    DockSources                   => "dock-sources";
    DockAudioMixer                => "dock-audio-mixer";
    DockControls                  => "dock-controls";
    DockProperties                => "dock-properties";
    DockFilters                   => "dock-filters";
    ProjectUnavailableTitle       => "project-unavailable-title";
    ProjectUnavailableBody        => "project-unavailable-body";
    ProjectUnavailableDismiss     => "project-unavailable-dismiss";
    FiltersNoSelection            => "filters-no-selection";
    FiltersPictureTab             => "filters-picture-tab";
    FiltersSoundTab               => "filters-sound-tab";
    FiltersOnTheSource            => "filters-on-the-source";
    FiltersEmpty                  => "filters-empty";
    FiltersAdd                    => "filters-add";
    FiltersRemove                 => "filters-remove";
    FiltersMoveUp                 => "filters-move-up";
    FiltersMoveDown               => "filters-move-down";
    FiltersChromaKey              => "filters-chroma-key";
    FiltersChromaKeyColour        => "filters-chroma-key-colour";
    FiltersChromaKeyCustom        => "filters-chroma-key-custom";
    FiltersChromaKeyThreshold     => "filters-chroma-key-threshold";
    FiltersChromaKeySmoothing     => "filters-chroma-key-smoothing";
    FiltersChromaKeyGreen         => "filters-chroma-key-green";
    FiltersChromaKeyBlue          => "filters-chroma-key-blue";
    FiltersChromaKeyCustomMethod  => "filters-chroma-key-custom-method";
    FiltersColorCorrection        => "filters-color-correction";
    FiltersBrightness             => "filters-brightness";
    FiltersContrast               => "filters-contrast";
    FiltersSaturation             => "filters-saturation";
    FiltersHue                    => "filters-hue";
    FiltersGamma                  => "filters-gamma";
    FiltersOpacity                => "filters-opacity";
    FiltersLumaKey                => "filters-luma-key";
    FiltersLumaKeyMin             => "filters-luma-key-min";
    FiltersLumaKeyMinSmoothing    => "filters-luma-key-min-smoothing";
    FiltersLumaKeyMax             => "filters-luma-key-max";
    FiltersLumaKeyMaxSmoothing    => "filters-luma-key-max-smoothing";
    FiltersOnTheChannel           => "filters-on-the-channel";
    FiltersOnTheSourceSound       => "filters-on-the-source-sound";
    FiltersNoiseSuppression       => "filters-noise-suppression";
    FiltersNoiseSuppressionAbout  => "filters-noise-suppression-about";
    FiltersNoiseGate              => "filters-noise-gate";
    FiltersGateOpen               => "filters-gate-open";
    FiltersGateClose              => "filters-gate-close";
    FiltersGateHold               => "filters-gate-hold";
    FiltersCompressor             => "filters-compressor";
    FiltersLimiter                => "filters-limiter";
    FiltersThreshold              => "filters-threshold";
    FiltersRatio                  => "filters-ratio";
    FiltersAttack                 => "filters-attack";
    FiltersRelease                => "filters-release";
    FiltersOutputGain             => "filters-output-gain";
    PropertiesNoSelection         => "properties-no-selection";
    PropertiesName                => "properties-name";
    PropertiesKind                => "properties-kind";
    PropertiesPosition            => "properties-position";
    PropertiesSize                => "properties-size";
    PropertiesRotation            => "properties-rotation";
    PropertiesVisible             => "properties-visible";
    PropertiesLocked              => "properties-locked";
    PropertiesYes                 => "properties-yes";
    PropertiesNo                  => "properties-no";
    PropertiesColour              => "properties-colour";
    PropertiesOpacity             => "properties-opacity";
    PropertiesStrokes             => "properties-strokes";
    PropertiesSurface             => "properties-surface";
    PropertiesText                => "properties-text";
    PropertiesTextMode            => "properties-text-mode";
    PropertiesTextModeStatic      => "properties-text-mode-static";
    PropertiesTextModeClock       => "properties-text-mode-clock";
    PropertiesTextModeTimer       => "properties-text-mode-timer";
    PropertiesTextFormat          => "properties-text-format";
    PropertiesTimer               => "properties-timer";
    PropertiesTimerStart          => "properties-timer-start";
    PropertiesTimerStop           => "properties-timer-stop";
    PropertiesTimerReset          => "properties-timer-reset";
    PropertiesFont                => "properties-font";
    PropertiesFontDefault         => "properties-font-default";
    PropertiesFontBrowse          => "properties-font-browse";
    PropertiesFontFilter          => "properties-font-filter";
    PropertiesFontSize            => "properties-font-size";
    PropertiesAlignment           => "properties-alignment";
    PropertiesAlignLeft           => "properties-align-left";
    PropertiesAlignCentre         => "properties-align-centre";
    PropertiesAlignRight          => "properties-align-right";
    PropertiesMonitor             => "properties-monitor";
    PropertiesPortalRemembered    => "properties-portal-remembered";
    PropertiesPortalAsks          => "properties-portal-asks";
    PropertiesStream              => "properties-stream";
    PropertiesDesktopPosition     => "properties-desktop-position";
    PropertiesDesktopSize         => "properties-desktop-size";
    PropertiesProcess             => "properties-process";
    PropertiesTitle               => "properties-title";
    PropertiesWindow              => "properties-window";
    PropertiesFile                => "properties-file";
    PropertiesCrop                => "properties-crop";
    PropertiesCropLeft            => "properties-crop-left";
    PropertiesCropTop             => "properties-crop-top";
    PropertiesCropRight           => "properties-crop-right";
    PropertiesCropBottom          => "properties-crop-bottom";
    PropertiesCamera              => "properties-camera";
    PropertiesCameraMode          => "properties-camera-mode";
    PropertiesCameraModeAutomatic => "properties-camera-mode-automatic";
    PropertiesUrl                 => "properties-url";
    PropertiesTransport           => "properties-transport";
    PropertiesReconnect           => "properties-reconnect";
    PropertiesReconnectOff        => "properties-reconnect-off";
    PropertiesReconnectSeconds    => "properties-reconnect-seconds";
    PropertiesLoop                => "properties-loop";
    PropertiesPlayback            => "properties-playback";
    PropertiesPlay                => "properties-play";
    PropertiesPause               => "properties-pause";
    AudioEmpty                    => "audio-empty";
    AudioMute                     => "audio-mute";
    AudioUnmute                   => "audio-unmute";
    AudioMonitorOff               => "audio-monitor-off";
    AudioMonitorOn                => "audio-monitor-on";
    AudioMonitorUnavailable       => "audio-monitor-unavailable";
    AudioKindOutput               => "audio-kind-output";
    AudioKindInput                => "audio-kind-input";
    AudioDeviceDefault            => "audio-device-default";
    AudioNoDevices                => "audio-no-devices";
    AudioFilters                  => "audio-filters";
    AudioClipped                  => "audio-clipped";
    ControlStartRecording         => "control-start-recording";
    ControlPauseRecording         => "control-pause-recording";
    ControlResumeRecording        => "control-resume-recording";
    ControlStopRecording          => "control-stop-recording";
    ControlSettings               => "control-settings";
    SettingsTitle                 => "settings-title";
    SettingsPageGeneral           => "settings-page-general";
    SettingsPageRecording         => "settings-page-recording";
    SettingsPageHotkeys           => "settings-page-hotkeys";
    HotkeyToggleRecording         => "hotkey-toggle-recording";
    HotkeyTogglePause             => "hotkey-toggle-pause";
    HotkeyFullscreen              => "hotkey-fullscreen";
    HotkeyOpenSettings            => "hotkey-open-settings";
    HotkeyToggleStreaming         => "hotkey-toggle-streaming";
    HotkeyPushToTalk              => "hotkey-push-to-talk";
    HotkeyPushToMute              => "hotkey-push-to-mute";
    HotkeyToggleMute              => "hotkey-toggle-mute";
    HotkeySwitchScene             => "hotkey-switch-scene";
    HotkeySectionGeneral          => "hotkey-section-general";
    HotkeySectionAudio            => "hotkey-section-audio";
    HotkeySectionScenes           => "hotkey-section-scenes";
    HotkeyPressAKey               => "hotkey-press-a-key";
    HotkeyNone                    => "hotkey-none";
    HotkeyConflict                => "hotkey-conflict";
    HotkeyHint                    => "hotkey-hint";
    SettingsPageVideo             => "settings-page-video";
    SettingsAudioOpusNeeds48k     => "settings-audio-opus-needs-48k";
    SettingsAudioWhileRecording   => "settings-audio-while-recording";
    SettingsAudioStereo           => "settings-audio-stereo";
    SettingsAudioMono             => "settings-audio-mono";
    SettingsAudioChannels         => "settings-audio-channels";
    SettingsAudioMonitorDevice    => "settings-audio-monitor-device";
    SettingsAudioMonitorNone      => "settings-audio-monitor-none";
    SettingsAudioMonitorFeedback  => "settings-audio-monitor-feedback";
    SettingsAudioSampleRate       => "settings-audio-sample-rate";
    SettingsPageAudio             => "settings-page-audio";
    SettingsVideoCanvas           => "settings-video-canvas";
    SettingsVideoCanvasFixed      => "settings-video-canvas-fixed";
    SettingsVideoOutput           => "settings-video-output";
    SettingsVideoFps              => "settings-video-fps";
    SettingsLanguage              => "settings-language";
    SettingsTheme                 => "settings-theme";
    SettingsRecordingDirectory    => "settings-recording-directory";
    SettingsRecordingNamePrefix   => "settings-recording-name-prefix";
    SettingsRecordingNameExample  => "settings-recording-name-example";
    SettingsRecordingEncoder      => "settings-recording-encoder";
    SettingsEncoderUnavailable    => "settings-encoder-unavailable";
    SettingsEncoderSoftwareCost   => "settings-encoder-software-cost";
    SettingsRecordingBitRate      => "settings-recording-bit-rate";
    SettingsRecordingKeyframes    => "settings-recording-keyframes";
    SettingsRecordingAudioCodec   => "settings-recording-audio-codec";
    SettingsRecordingAudioBitRate => "settings-recording-audio-bit-rate";
    SettingsRecordingFormat       => "settings-recording-format";
    SettingsRecordingSplit        => "settings-recording-split";
    SettingsRecordingSplitOff     => "settings-recording-split-off";
    SettingsRecordingSplitTime    => "settings-recording-split-time";
    SettingsRecordingSplitSize    => "settings-recording-split-size";
    SettingsRecordingSplitHls     => "settings-recording-split-hls";
    SettingsRecordingWhileRunning => "settings-recording-while-running";
    SettingsFpsWhileRecording     => "settings-fps-while-recording";
    ActionApply                   => "action-apply";
    ActionBrowse                  => "action-browse";
    ActionOk                      => "action-ok";
    ExitWhileRecordingTitle       => "exit-while-recording-title";
    ExitWhileRecordingBody        => "exit-while-recording-body";
    ExitStopAndQuit               => "exit-stop-and-quit";
    ExitKeepRecording             => "exit-keep-recording";
    StatusRecording               => "status-recording";
    StatusRecordingFailed         => "status-recording-failed";
    StatusRecordingPaused         => "status-recording-paused";
    StatusReady                   => "status-ready";
    StatusGpuProcess              => "status-gpu-process";
    StatusGpuDevice               => "status-gpu-device";
    StatusGpuUnavailable          => "status-gpu-unavailable";
    SceneNameEmpty                => "scene-name-empty";
    SceneNameDuplicate            => "scene-name-duplicate";
    SceneAdd                      => "scene-add";
    SceneRemove                   => "scene-remove";
    SceneDuplicate                => "scene-duplicate";
    SceneMoveUp                   => "scene-move-up";
    SceneMoveDown                 => "scene-move-down";
    SourceSelectedScene           => "source-selected-scene";
    SourceEmpty                   => "source-empty";
    SourceAdd                     => "source-add";
    SourceRemove                  => "source-remove";
    SourceNameEmpty               => "source-name-empty";
    SourceNameDuplicate           => "source-name-duplicate";
    SourceMoveUp                  => "source-move-up";
    SourceMoveDown                => "source-move-down";
    SourceAddTitle                => "source-add-title";
    SourceType                    => "source-type";
    SourceCameraTitle             => "source-camera-title";
    SourceCameraPrompt            => "source-camera-prompt";
    SourceCameraNone              => "source-camera-none";
    SourceDisplayTitle            => "source-display-title";
    SourceDisplayPrompt           => "source-display-prompt";
    SourceDisplayMonitor          => "source-display-monitor";
    SourceDisplayMonitorPrimary   => "source-display-monitor-primary";
    MenuSettings                  => "menu-settings";
    MenuShowRecordings            => "menu-show-recordings";
    StatusMemory                  => "status-memory";
    StatusMemoryResident          => "status-memory-resident";
    StatusMemoryBoth              => "status-memory-both";
    SourceDisplayNone             => "source-display-none";
    SourceWindowTitle             => "source-window-title";
    SourceWindowPrompt            => "source-window-prompt";
    SourceWindowRow               => "source-window-row";
    SourceWindowNone              => "source-window-none";
    SourceKindColor               => "source-kind-color";
    SourceKindDrawing             => "source-kind-drawing";
    SourceKindText                => "source-kind-text";
    SourceKindMediaFile           => "source-kind-media-file";
    SourceMediaFileFilter         => "source-media-file-filter";
    SourceImageFilter             => "source-image-filter";
    SourceEnded                   => "source-ended";
    AudioKindMediaFile            => "audio-kind-media-file";
    AudioKindStream               => "audio-kind-stream";
    DrawingToolSelect             => "drawing-tool-select";
    DrawingToolPen                => "drawing-tool-pen";
    DrawingToolHighlighter        => "drawing-tool-highlighter";
    DrawingToolEraser             => "drawing-tool-eraser";
    DrawingWidth                  => "drawing-width";
    DrawingWidthThin              => "drawing-width-thin";
    DrawingWidthMedium            => "drawing-width-medium";
    DrawingWidthThick             => "drawing-width-thick";
    DrawingUndo                   => "drawing-undo";
    DrawingClear                  => "drawing-clear";
    SourceDisconnected            => "source-disconnected";
    SourceReopen                  => "source-reopen";
    SourceKindDisplayCapture      => "source-kind-display-capture";
    SourceKindWindowCapture       => "source-kind-window-capture";
    SourceStreamTitle             => "source-stream-title";
    SourceStreamPrompt            => "source-stream-prompt";
    SourceStreamTrying            => "source-stream-trying";
    SourceKindRtsp                => "source-kind-rtsp";
    SourceKindVideoCapture        => "source-kind-video-capture";
    SourceKindImage               => "source-kind-image";
    ActionAdd                     => "action-add";
    ActionCancel                  => "action-cancel";
    ActionBack                    => "action-back";
    PreviewNoFrame                => "preview-no-frame";
    PreviewScaleDecrease          => "preview-scale-decrease";
    PreviewScaleIncrease          => "preview-scale-increase";
    PreviewScaleFit               => "preview-scale-fit";
    PreviewFitWorkspace           => "preview-fit-workspace";
    PreviewResetView              => "preview-reset-view";
    PreviewScaleOptions           => "preview-scale-options";
    ControlStartStreaming           => "control-start-streaming";
    ControlStopStreaming            => "control-stop-streaming";
    StatusStreaming                 => "status-streaming";
    SettingsPageStreaming           => "settings-page-streaming";
    SettingsStreamingWhileRunning   => "settings-streaming-while-running";
    SettingsStreamingServer         => "settings-streaming-server";
    SettingsStreamingKey            => "settings-streaming-key";
    SettingsStreamingKeyShow        => "settings-streaming-key-show";
    SettingsStreamingKeyHide        => "settings-streaming-key-hide";
    SettingsStreamingKeyStored      => "settings-streaming-key-stored";
    SettingsStreamingEncoder        => "settings-streaming-encoder";
    SettingsStreamingBitRate        => "settings-streaming-bit-rate";
    SettingsStreamingBitRateNote    => "settings-streaming-bit-rate-note";
    SettingsStreamingKeyframes      => "settings-streaming-keyframes";
    SettingsStreamingAudioCodec     => "settings-streaming-audio-codec";
    SettingsStreamingAudioBitRate   => "settings-streaming-audio-bit-rate";
    SettingsStreamingReconnect        => "settings-streaming-reconnect";
    SettingsStreamingReconnectNever   => "settings-streaming-reconnect-never";
    SettingsStreamingReconnectAfter   => "settings-streaming-reconnect-after";
    StatusStreamingReconnecting       => "status-streaming-reconnecting";
    StatusLag                         => "status-lag";
    StatusLagNoticeable               => "status-lag-noticeable";
    StatusLagSevere                   => "status-lag-severe";
    DockStats                         => "dock-stats";
    StatsNothingRunning               => "stats-nothing-running";
    StatsSubject                      => "stats-subject";
    StatsRate                         => "stats-rate";
    StatsBusy                         => "stats-busy";
    StatsQueue                        => "stats-queue";
    StatsIdle                         => "stats-idle";
    StatsCompositor                   => "stats-compositor";
    StatsRecording                    => "stats-recording";
    StatsBroadcast                    => "stats-broadcast";
    StatsErrors                       => "stats-errors";
    StatsCpu                          => "stats-cpu";
    StatsDiskAvailable                => "stats-disk-available";
    StatsDiskFullIn                   => "stats-disk-full-in";
    StatsMemory                       => "stats-memory";
    StatsFps                          => "stats-fps";
    StatsFrameTime                    => "stats-frame-time";
    StatsMissedFrames                 => "stats-missed-frames";
    StatsMissedFramesHint             => "stats-missed-frames-hint";
    StatsSkippedFrames                => "stats-skipped-frames";
    StatsSkippedFramesHint            => "stats-skipped-frames-hint";
    StatsOutput                       => "stats-output";
    StatsStatus                       => "stats-status";
    StatsLostFrames                   => "stats-lost-frames";
    StatsDataOutput                   => "stats-data-output";
    StatsBitrate                      => "stats-bitrate";
    StatsStatusStopped                => "stats-status-stopped";
    StatsStatusRecording              => "stats-status-recording";
    StatsStatusPaused                 => "stats-status-paused";
    StatsStatusLive                   => "stats-status-live";
    StatsStatusReconnecting           => "stats-status-reconnecting";
    StatsDetails                      => "stats-details";
    StatsReset                        => "stats-reset";
    StatsResetHint                    => "stats-reset-hint";
    StatsHours                        => "stats-hours";
    StatsMinutes                      => "stats-minutes";
}
