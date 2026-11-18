//! Every text the editor shows. A [`Message`] says what to tell the user; each language is one table
//! with an exhaustive `match`, so a language lacking a string does not compile. Adding a language
//! is one table and one line in [`Language`]. Detail from an encoder, FFmpeg or a media probe
//! travels as [`Message::Technical`] or as the argument of a translated prefix, in English. A
//! refused edit has a message of its own for each reason.

mod en;
mod vi;

use crate::package_import::ImportProblem;

/// A display language of the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    English,
    Vietnamese,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::Vietnamese];

    /// How the settings file names the language.
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Vietnamese => "vi",
        }
    }

    /// The language with settings code `code`, if the editor has it.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|language| language.code() == code)
    }

    /// The name of the language in the language itself, never translated.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Vietnamese => "Tiếng Việt",
        }
    }

    pub fn text(self, message: &Message) -> String {
        match self {
            Self::English => en::text(message),
            Self::Vietnamese => vi::text(message),
        }
    }

    pub fn t(self, message: Message) -> String {
        self.text(&message)
    }
}

/// A media stream, in messages about a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
}

/// A track named in a message: as the timeline labels it, or by position when it does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackLabel {
    Video(usize),
    Audio(usize),
    Sticker(usize),
    Text(usize),
    Position(usize),
}

/// The edge of a clip that an edit was refused on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipEdge {
    Start,
    End,
}

/// What a selected clip was needed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipAction {
    Split,
    Delete,
    Change,
    Mute,
    Trim,
    AddEffect,
    RemoveEffect,
    AddTransition,
}

/// Everything the editor can say. Stored instead of finished text, so what is on screen follows a
/// language change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Text that is not translated: an encoder name, or FFmpeg or media probe detail.
    Technical(String),

    // Status line.
    Ready,
    Loading(String),
    RefusedLine(String),
    PreviewLine(String),
    AudioLine(String),
    NoSoundLine(String),
    AudioGapLine(u64),
    FinishDragFirst,
    ExportRunning,
    ExportAlreadyRunning,
    NoNeighbour,
    NothingAtPlayhead,
    NothingToPlay,
    NothingToUndo,
    NothingToRedo,
    Saved(String),
    Opened(String),
    Exporting,
    Added(String),
    AddedWithLength { name: String, seconds: String },
    AddedNoLength { name: String, seconds: i64 },
    StreamUnusable { name: String, stream: StreamKind, reason: String },
    StreamMissing { name: String, stream: StreamKind },
    Imported(String),
    NoUsableStream(String),
    SelectClip(ClipAction),
    ExpressionDropped,

    // Refused edits, one for each reason the engine has. Clip numbers count from 1.
    EditTrackNotFound(usize),
    EditClipNotFound { track: TrackLabel, clip: usize },
    EditEffectNotFound { track: TrackLabel, clip: usize, effect: usize },
    EditTransitionNotFound { track: TrackLabel, after_clip: usize },
    EditTrackKindMismatch { from: engine::TrackKind, to: engine::TrackKind },
    EditNoRoom(TrackLabel),
    EditOverlap { track: TrackLabel, start: String, end: String },
    EditTimeOverflow { track: TrackLabel, clip: usize },
    EditTransitionOnEdge { track: TrackLabel, edge: ClipEdge },
    EditBeforeSourceStart(TrackLabel),
    EditTrackLocked(TrackLabel),
    EditSpeedOverlap { track: TrackLabel, end: String, next: String },
    EditSplitOutsideClip { track: TrackLabel, at: String },
    EditSplitInsideFade { track: TrackLabel, at: String },
    EditSplitInsideTransition { track: TrackLabel, at: String },
    EditSplitInsideHold { track: TrackLabel, at: String },
    /// The validator's own list, which stays in English under a translated prefix.
    EditInvalid(String),
    LanguageSaveFailed(String),
    PanelSizesSaveFailed(String),
    ResizePanelHint,

    // Background work.
    DialogOpen,
    LoadRunning,
    LoadDropped,
    OpenCancelled,
    OpenFailed(String),
    SaveFailed(String),
    TaskFailed(String),

    // Prompts.
    PromptDiscardForNew,
    PromptDiscardForOpen,
    PromptDiscardForClose,
    PromptCancelExport,

    // Export.
    NothingToExport,
    ExportProgress { done: i64, total: i64 },
    ExportFinished { frames: i64, path: String, encoder: String },
    ExportFinishedSilent { frames: i64, path: String, encoder: String },
    ExportCancelled,
    ExportFailed(String),
    /// The renderer is not beside the editor; holds its path.
    ExportToolMissing(String),
    /// The renderer ended without a result.
    ExportChildCrashed { code: String, stderr: String },
    /// The renderer beside the editor speaks another version of the progress protocol.
    ExportRendererMismatch,
    /// The renderer's last line was unreadable after every frame; holds the temporary files kept.
    ExportResultUnknown(String),
    EncoderAuto,
    EncoderNeedsGpl(String),
    EncoderMissing(String),
    /// The encoder is being tested to see whether it opens on this machine.
    EncoderProbing(String),
    /// The build has the encoder but it does not open on this machine.
    EncoderUnavailable(String),

    // File dialogs.
    FilterMedia,
    FilterAudio,
    FilterProject,
    FilterAllFiles,

    // Top bar.
    Dirty,
    Untitled,
    MenuFile,
    MenuNewProject,
    MenuOpen,
    MenuSave,
    MenuSaveAs,
    MenuLanguage,
    MenuTheme,
    ThemeSystem,
    ThemeDark,
    ThemeLight,
    ThemeSaveFailed(String),
    MenuAgentAccess,
    AgentConnected,
    ProjectInUse(String),
    AgentAccessFailed(String),
    MenuAutosave,
    AutosaveOff,
    /// Autosave every so many seconds.
    AutosaveEvery(u64),
    AutosaveSettingFailed(String),
    /// Writing the autosave copy failed.
    AutosaveFailed(String),

    // The package manager.
    MenuPackageManager,
    /// The File menu option that copies the packs a project uses next to it on every save.
    MenuSaveWithPacks,
    PackManagerTitle,
    PackTabName(crate::package_manager::PackTab),
    PackColumnName,
    PackColumnVersion,
    PackColumnSource,
    PackColumnItems,
    PackColumnSize,
    PackColumnEnabled,
    PackSourceShipped,
    PackSourceUser,
    /// A shader package kept out of the effect lists.
    PackInternal,
    PackShadowed,
    PackReadOnly,
    /// Why packs cannot be installed, removed or turned on or off now.
    PackBusyExporting,
    PackInstallZip,
    PackRemove,
    PackOpenFolder,
    PackRefresh,
    PackFilter,
    PackNone,
    PackRemoveQuestion(String),
    /// How many places of the open project use the pack about to be removed.
    PackUsedBy(usize),
    PackUseClip { track: usize, clip: usize },
    PackUseTransition { track: usize, after_clip: usize },
    PackRemoved(String),
    PackRemoveFailed(String),
    PackToggleFailed(String),
    PackOpenFolderFailed(String),
    PackSaveWithPacksFailed(String),
    PackBackupFailed(String),
    /// The open project uses packs that are not installed but are in the copy beside it.
    PackRestoreOffer(usize),
    PackRestoreInstall,
    PackRestoreDismiss,
    PackRestored(usize),
    PackRestoreFailed(String),
    NoUserPackageFolder,

    // Preview proxies.
    MenuProxy,
    ProxyOff,
    /// Proxies for videos taller than 1080 pixels.
    ProxyAuto,
    ProxyOn,
    /// Stops the proxies queued or being made.
    ProxyCancel,
    /// Makes again the proxies that failed or were stopped.
    ProxyRetry,
    /// Proxies left to make, and the progress of the running one in percent.
    ProxyMaking { left: usize, percent: u32 },
    /// The short label on a video that has a proxy, or is getting one.
    ProxyBadge,
    ProxyQueued,
    /// Making the proxy, this many percent done.
    ProxyRunning(u32),
    ProxyReady,
    ProxyFailed(String),
    ProxyCancelled,
    ProxySettingFailed(String),

    // Recovery of autosaves after an unclean exit.
    RecoveryTitle,
    RecoveryIntro,
    RecoveryRestore,
    RecoveryDiscard,
    RecoveryLater,
    /// The autosave was written so many seconds ago.
    RecoveryAgo(u64),
    RecoveryOriginalNewer,
    /// Restored, with the project file it belongs to, empty for one never saved.
    RecoveryRestored(String),
    RecoveryFailed(String),
    /// A restore must not replace unsaved changes or race a running task.
    RecoveryBusy,
    /// Another editor restored or discarded the autosave first.
    RecoveryTaken(String),
    /// Autosaves are kept this many days.
    RecoveryKeptDays(u64),
    Cancel,
    ExportVideo,

    // Left panel.
    TabMedia,
    TabAudio,
    TabStickers,
    SearchStickers,
    AllCategories,
    StickerLibraryEmpty,
    StickerAddHint,
    /// The hover line of an animated sticker: one loop in milliseconds, and how many times it plays (0 is forever).
    AnimatedSticker { loop_ms: u64, loops: u32 },
    StickerPackInstalled { name: String, replaced: bool },
    FontPackInstalled { name: String, replaced: bool },
    ClipAnimationPackInstalled { name: String, replaced: bool },
    TextStylePackInstalled { name: String, replaced: bool },
    /// The Sticker tab has not finished reading the installed packs.
    ReadingStickerPacks,
    /// How many sticker packs or stickers could not be read.
    StickerPackProblems(usize),
    EditSpeedOfImage(TrackLabel),
    EditKeyframeOutsideClip(TrackLabel),
    EditKeyframeNotFound(TrackLabel),
    EditNoPlacement(TrackLabel),
    /// A marker that is not there, numbered from 1.
    EditMarkerNotFound(usize),
    /// The Text tab of the left panel, and the Text part of the inspector.
    TabText,
    AddText,
    TextAddHint,
    TextStyles,
    SubtitleStyles,
    ReadingTextStylePacks,
    TextStyleLibraryEmpty,
    TextStylePackProblems(usize),
    /// The combo of the text inspector that applies a style of an installed pack.
    ApplyTextStyle,
    /// What a new text clip says.
    DefaultTextContent,
    TextAdded,
    EditSpeedOfText(TrackLabel),
    EditReverseOfStill(TrackLabel),
    FieldTextContent,
    FieldFont,
    DefaultFont,
    ChooseFontFile,
    FilterFont,
    /// The font list is not read yet.
    LoadingFonts,
    SearchFonts,
    FontSourceAll,
    FontSourceSystem,
    FontSourcePack,
    NoFontFound,
    /// The font a text asks for is not installed and the default one draws it.
    FontMissing(String),
    /// A texture picture a text paints with cannot be read, so its layer is left out.
    TextureMissing(String),
    /// The project changed while the font file dialog was up, so the file chosen is not used.
    FontTargetChanged,
    /// The button that imports an SRT file as a subtitle track.
    ImportSubtitles,
    FilterSubtitles,
    /// A subtitle track was added with this many cues; this many cues of the file were left out.
    SubtitlesImported { cues: usize, skipped: usize },
    /// The file holds no cue that can be read.
    SubtitlesNone,
    /// The subtitle file could not be read or is not UTF-8 text.
    SubtitleFileUnreadable(String),
    SubtitleTrack,
    /// The combo of the style of a subtitle track.
    SubtitleTrackStyle,
    /// Shown in that combo while the track has no style.
    SubtitleStyleNone,
    /// The check box that keeps the style of one cue when the track's style changes.
    SubtitleOwnStyle,
    /// The check box that turns karaoke on for a cue.
    Karaoke,
    KaraokeColor,
    KaraokeBox,
    KaraokeScale,
    /// The colour of the word being spoken.
    KaraokeActiveColor,
    ExportSubtitles,
    SubtitlesExported(String),
    SubtitleExportFailed(String),
    SubtitleExportTargetChanged,
    FieldTextSize,
    FieldTextColor,
    TextOutline,
    TextBackground,
    TextShadow,
    TextGlow,
    FieldOuterColor,
    FieldGlowRadius,
    FieldIntensity,
    FieldFalloff,
    FieldColor,
    FieldThickness,
    FieldPadding,
    FieldCornerRadius,
    FieldShadowOffset,
    FieldBlur,
    FieldTextureMix,
    LayerLimit(usize),
    TextLayers,
    TextFill,
    PaintSolid,
    PaintGradient,
    PaintTexture,
    FieldAngle,
    AddGradientStop,
    FieldPosition,
    TexturePicture,
    ChooseTextureFile,
    FitStretch,
    FitTile,
    FitMirror,
    FitGlyph,
    FieldCells,
    FlipX,
    FlipY,
    TextureBase,
    BaseNone,
    BaseSolid,
    BaseGradient,
    GradientPerGlyph,
    FieldScale,
    TextOutlines,
    TextShadows,
    TextInnerShadows,
    TextInnerShadow,
    TextBloom,
    AddLayer,
    MoveLayerUp,
    MoveLayerDown,
    FieldDistance,
    FieldStrength,
    FieldSpread,
    FieldShiftX,
    FieldShiftY,
    ShadowOutlines,
    FilterPicture,
    TextureTargetChanged,
    FieldAlign,
    AlignLeft,
    AlignCenter,
    AlignRight,
    FieldLineSpacing,
    FieldFontWeight,
    TextItalic,
    FieldTextTransform,
    TransformNone,
    TransformUppercase,
    TransformLowercase,
    FieldLetterSpacing,
    /// The tab of the clip card with everything but the animations.
    ClipTabProperties,
    /// The clip tab of the clip's picture: place, motion, animations, effects.
    ClipTabPicture,
    /// The general clip tab of a sound clip, whose sound tab is named after what it is.
    ClipTabGeneral,
    TabAnimation,
    /// A hold was set on a cut that had a transition, so the transition went.
    HoldRemovedTransition,
    /// A transition was added on a cut that had holds, so they became plain animations.
    TransitionReleasedHold,
    /// Under a negative animation duration: the still lasts that many seconds.
    AnimationHoldNote(String),
    /// Hover text of the animation duration.
    AnimationDurationHint,
    AnimationEnter,
    AnimationExit,
    AnimationRepeat,
    AnimationNone,
    FieldAnimationDuration,
    /// The name of a text animation preset, by its id.
    AnimationPreset(&'static str),
    /// A preset of a pack that is not installed, by what the clip stores.
    AnimationPackMissing(String),
    TabEffects,
    Transitions,
    ImportFromComputer,
    ImportPackage,
    FilterPackage,
    /// Replace the installed package with this name?
    PromptOverwritePackage(String),
    PackageInstalled { name: String, transition: bool, replaced: bool },
    PackageImportRefused(ImportProblem),
    AudioPackInstalled { name: String, replaced: bool },
    /// The heading of what was imported in this session, in the Audio tab.
    SessionAudio,
    /// The heading of the installed packs in the Audio tab.
    InstalledPacks,
    ReadingAudioPacks,
    /// The Audio tab has neither session files nor installed packs.
    AudioLibraryEmpty,
    AudioPackProblems(usize),
    PreviewSound,
    StopPreview,
    AddSound,
    /// The hint under the search of the Audio tab's packs.
    AudioAddHint,
    SoundPreviewFailed(String),
    SearchMedia,
    SearchAudio,
    SearchEffects,
    LibraryEmpty,
    NoSearchMatch,
    AddHint,
    ReadingPackages,
    NoPackages(String),
    AddToClip,
    PackageProblems(usize),
    /// The tooltip of a tile whose preview could not be made, with the reason.
    PreviewFailed(String),
    /// The tooltip of a tile when previews cannot be made at all, with the reason.
    PreviewUnavailable(String),

    // Inspector.
    /// Inspector titles by what the selected clip is.
    ClipTitleVideo,
    ClipTitleAudio,
    ClipTitleImage,
    ClipTitleSticker,
    ClipTitleText,
    TransitionTitle,
    ProjectTitle,
    FieldSize,
    FieldFrameRate,
    FieldAspect,
    FieldDuration,
    FieldSpeed,
    FieldStart,
    FieldSourceIn,
    FieldVideoDuration,
    FieldPlacement,
    FieldOpacity,
    FieldRotation,
    FieldPositionX,
    FieldPositionY,
    FieldWidth,
    FieldEasing,
    KeyframeAdd,
    KeyframeRemove,
    KeyframePrevious,
    KeyframeNext,
    EasingLinear,
    EasingEaseIn,
    EasingEaseOut,
    EasingEaseInOut,
    DropOnTimeline,
    OutputSizeCustom,
    FrameRateCustom,
    Apply,
    FieldVolume,
    SelectClipHint,
    Mute,
    /// The check box and menu item that play a clip backwards.
    Reverse,
    FadeIn,
    FadeOut,
    FieldDenoise,
    FieldMatte,
    MatteOff,
    MatteQuality,
    MatteQualityHint,
    MatteFast,
    MatteFastHint,
    MatteModelMissing,
    MatteImportZip,
    MatteFetch,
    MatteFetchUnavailable,
    MatteFetching,
    MatteFetchCancel,
    MatteFetchRetry,
    MatteThreshold,
    MatteFeather,
    MatteEdge,
    MatteTemporal,
    MatteTemporalHint,
    MatteInvert,
    AddCensor,
    AddCensorHint,
    CensorAdded,
    ClipTitleCensor,
    CensorShape,
    CensorShapeRect,
    CensorShapeEllipse,
    CensorFeather,
    CensorMode,
    CensorBlur,
    CensorPixelate,
    CensorSolid,
    CensorStrength,
    CensorBlock,
    CensorColor,
    MatteImported,
    MatteImportFailed(String),
    FilterMatteModel,
    DenoiseOff,
    DenoiseVoice,
    DenoiseGeneral,
    DenoiseVoiceHint,
    DenoiseGeneralHint,
    DenoiseStrength,
    FieldVoiceChange,
    VoiceNormal,
    VoiceChipmunk,
    VoiceDeep,
    VoiceRobot,
    FieldPitch,
    FieldBeats,
    DetectBeats,
    DetectBeatsHint,
    ClearBeats,
    CancelBeats,
    /// While beats are detected, in whole percent.
    BeatsDetecting(u32),
    /// The beats of a clip: how many and the tempo, as written by `tempo_text`.
    BeatsSummary { count: usize, tempo: String },
    BeatsNone,
    BeatsFound { count: usize, tempo: String },
    BeatsNotFound,
    /// A detection finished after an undo: its beats wait in the cache so the redo step stays.
    BeatsKeptForRedo,
    BeatsFailed(String),
    MarkerAddShortcut,
    MarkerHint,
    MarkerRemove,
    MarkerLabel,
    MarkerColor,
    MarkerDefaultColor,
    NoEffectsYet,
    TrackLockedNote,
    SpeedBlockedByEndTransition,
    Remove,
    Reset,
    PackageParamsUnreadable,
    Variables(String),

    // Player.
    Player,
    Fullscreen,
    ExitFullscreen,
    PlayShortcut,
    StopShortcut,

    // Timeline.
    UndoShortcut,
    RedoShortcut,
    SplitShortcut,
    TrimBefore,
    TrimAfter,
    SnapToggle,
    DeleteShortcut,
    ZoomIn,
    ZoomOut,
    AddTrack,
    AddVideoTrack,
    AddAudioTrack,
    AddStickerTrack,
    AddTextTrack,
    VideoTrackNumber(usize),
    AudioTrackNumber(usize),
    StickerTrackNumber(usize),
    TextTrackNumber(usize),
    AddClipFromFile,
    LockTrack,
    UnlockTrack,
    HideTrack,
    DeleteTrack,
    ShowTrack,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_import::EntryProblem;

    #[test]
    fn every_language_has_a_unique_code_that_reads_back() {
        for language in Language::ALL {
            assert_eq!(Language::from_code(language.code()), Some(language));
        }
        assert_eq!(Language::from_code("xx"), None);
    }

    /// A language lacking a string does not compile, as each table is an exhaustive match. This
    /// catches the other mistake: a text that was filled in with nothing.
    #[test]
    fn no_text_is_empty_in_any_language() {
        let name = || "a".to_owned();
        let all = [
            Message::Ready,
            Message::Loading(name()),
            Message::RefusedLine(name()),
            Message::FinishDragFirst,
            Message::Added(name()),
            Message::StreamMissing { name: name(), stream: StreamKind::Audio },
            Message::SelectClip(ClipAction::AddTransition),
            Message::PromptCancelExport,
            Message::ImportPackage,
            Message::FilterPackage,
            Message::FieldMatte,
            Message::MatteOff,
            Message::MatteQuality,
            Message::MatteQualityHint,
            Message::MatteFast,
            Message::MatteFastHint,
            Message::MatteModelMissing,
            Message::MatteImportZip,
            Message::MatteFetch,
            Message::MatteFetchUnavailable,
            Message::MatteFetching,
            Message::MatteFetchCancel,
            Message::MatteFetchRetry,
            Message::MatteThreshold,
            Message::MatteFeather,
            Message::MatteEdge,
            Message::AddCensor,
            Message::AddCensorHint,
            Message::CensorAdded,
            Message::ClipTitleCensor,
            Message::CensorShape,
            Message::CensorShapeRect,
            Message::CensorShapeEllipse,
            Message::CensorFeather,
            Message::CensorMode,
            Message::CensorBlur,
            Message::CensorPixelate,
            Message::CensorSolid,
            Message::CensorStrength,
            Message::CensorBlock,
            Message::CensorColor,
            Message::MatteTemporal,
            Message::MatteTemporalHint,
            Message::MatteInvert,
            Message::MatteImported,
            Message::MatteImportFailed(name()),
            Message::FilterMatteModel,
            Message::PromptOverwritePackage(name()),
            Message::PackageInstalled { name: name(), transition: true, replaced: false },
            Message::PackageImportRefused(ImportProblem::NotSupportedYet(name())),
            Message::PackageImportRefused(ImportProblem::SizeMismatch { entry: name() }),
            Message::PackageImportRefused(ImportProblem::RatioTooHigh { entry: name(), limit: 200 }),
            Message::PackageImportRefused(ImportProblem::Entry { entry: name(), problem: EntryProblem::ExtensionNotAllowed }),
            Message::AudioPackInstalled { name: name(), replaced: true },
            Message::TabStickers,
            Message::SearchStickers,
            Message::AllCategories,
            Message::StickerLibraryEmpty,
            Message::StickerAddHint,
            Message::AnimatedSticker { loop_ms: 600, loops: 0 },
            Message::AnimatedSticker { loop_ms: 1250, loops: 3 },
            Message::StickerPackInstalled { name: name(), replaced: true },
            Message::FontPackInstalled { name: name(), replaced: false },
            Message::ClipAnimationPackInstalled { name: name(), replaced: false },
            Message::TextStylePackInstalled { name: name(), replaced: true },
            Message::ReadingStickerPacks,
            Message::StickerPackProblems(2),
            Message::EditSpeedOfImage(TrackLabel::Video(1)),
            Message::EditKeyframeOutsideClip(TrackLabel::Video(1)),
            Message::EditKeyframeNotFound(TrackLabel::Video(1)),
            Message::EditNoPlacement(TrackLabel::Video(1)),
            Message::EditMarkerNotFound(2),
            Message::TabText,
            Message::AddText,
            Message::TextAddHint,
            Message::TextStyles,
            Message::SubtitleStyles,
            Message::ReadingTextStylePacks,
            Message::TextStyleLibraryEmpty,
            Message::TextStylePackProblems(2),
            Message::ApplyTextStyle,
            Message::DefaultTextContent,
            Message::TextAdded,
            Message::EditSpeedOfText(TrackLabel::Video(1)),
            Message::EditReverseOfStill(TrackLabel::Video(1)),
            Message::Reverse,
            Message::FieldTextContent,
            Message::FieldFont,
            Message::DefaultFont,
            Message::ChooseFontFile,
            Message::FilterFont,
            Message::LoadingFonts,
            Message::SearchFonts,
            Message::FontSourceAll,
            Message::FontSourceSystem,
            Message::FontSourcePack,
            Message::NoFontFound,
            Message::FontMissing(name()),
            Message::TextureMissing(name()),
            Message::FontTargetChanged,
            Message::ImportSubtitles,
            Message::FilterSubtitles,
            Message::SubtitlesImported { cues: 3, skipped: 1 },
            Message::SubtitlesNone,
            Message::SubtitleFileUnreadable(name()),
            Message::SubtitleTrack,
            Message::SubtitleTrackStyle,
            Message::SubtitleStyleNone,
            Message::SubtitleOwnStyle,
            Message::Karaoke,
            Message::KaraokeColor,
            Message::KaraokeBox,
            Message::KaraokeScale,
            Message::KaraokeActiveColor,
            Message::ExportSubtitles,
            Message::SubtitlesExported(name()),
            Message::SubtitleExportFailed(name()),
            Message::SubtitleExportTargetChanged,
            Message::FieldTextSize,
            Message::FieldTextColor,
            Message::TextOutline,
            Message::TextBackground,
            Message::TextShadow,
            Message::TextGlow,
            Message::FieldOuterColor,
            Message::FieldGlowRadius,
            Message::FieldIntensity,
            Message::FieldFalloff,
            Message::FieldColor,
            Message::FieldThickness,
            Message::FieldPadding,
            Message::FieldCornerRadius,
            Message::FieldShadowOffset,
            Message::FieldBlur,
            Message::FieldTextureMix,
            Message::LayerLimit(3),
            Message::TextLayers,
            Message::TextFill,
            Message::PaintSolid,
            Message::PaintGradient,
            Message::PaintTexture,
            Message::FieldAngle,
            Message::AddGradientStop,
            Message::FieldPosition,
            Message::TexturePicture,
            Message::ChooseTextureFile,
            Message::FitStretch,
            Message::FitTile,
            Message::FitMirror,
            Message::FitGlyph,
            Message::FieldCells,
            Message::FlipX,
            Message::FlipY,
            Message::TextureBase,
            Message::BaseNone,
            Message::BaseSolid,
            Message::BaseGradient,
            Message::GradientPerGlyph,
            Message::FieldScale,
            Message::TextOutlines,
            Message::TextShadows,
            Message::TextInnerShadows,
            Message::TextInnerShadow,
            Message::TextBloom,
            Message::AddLayer,
            Message::MoveLayerUp,
            Message::MoveLayerDown,
            Message::FieldDistance,
            Message::FieldStrength,
            Message::FieldSpread,
            Message::FieldShiftX,
            Message::FieldShiftY,
            Message::ShadowOutlines,
            Message::FilterPicture,
            Message::TextureTargetChanged,
            Message::FieldAlign,
            Message::AlignLeft,
            Message::AlignCenter,
            Message::AlignRight,
            Message::FieldLineSpacing,
            Message::FieldFontWeight,
            Message::TextItalic,
            Message::FieldTextTransform,
            Message::TransformNone,
            Message::TransformUppercase,
            Message::TransformLowercase,
            Message::FieldLetterSpacing,
            Message::TabAnimation,
            Message::ClipTabProperties,
            Message::ClipTabPicture,
            Message::ClipTabGeneral,
            Message::HoldRemovedTransition,
            Message::TransitionReleasedHold,
            Message::AnimationHoldNote("0.5".to_owned()),
            Message::AnimationDurationHint,
            Message::AnimationEnter,
            Message::AnimationExit,
            Message::AnimationRepeat,
            Message::AnimationNone,
            Message::FieldAnimationDuration,
            Message::AnimationPreset("fade"),
            Message::AnimationPreset("unknown-id"),
            Message::AnimationPackMissing(name()),
            Message::SessionAudio,
            Message::InstalledPacks,
            Message::ReadingAudioPacks,
            Message::AudioLibraryEmpty,
            Message::AudioPackProblems(2),
            Message::PreviewSound,
            Message::StopPreview,
            Message::AddSound,
            Message::AudioAddHint,
            Message::SoundPreviewFailed(name()),
            Message::PackageImportRefused(ImportProblem::Entry { entry: name(), problem: EntryProblem::Colon }),
            Message::MenuFile,
            Message::MenuTheme,
            Message::ThemeSystem,
            Message::ThemeDark,
            Message::ThemeLight,
            Message::ThemeSaveFailed(name()),
            Message::MenuAutosave,
            Message::MenuPackageManager,
            Message::MenuSaveWithPacks,
            Message::PackManagerTitle,
            Message::PackColumnName,
            Message::PackColumnVersion,
            Message::PackColumnSource,
            Message::PackColumnItems,
            Message::PackColumnSize,
            Message::PackColumnEnabled,
            Message::PackSourceShipped,
            Message::PackSourceUser,
            Message::PackInternal,
            Message::PackShadowed,
            Message::PackReadOnly,
            Message::PackBusyExporting,
            Message::PackInstallZip,
            Message::PackRemove,
            Message::PackOpenFolder,
            Message::PackRefresh,
            Message::PackFilter,
            Message::PackNone,
            Message::PackRemoveQuestion(name()),
            Message::PackUsedBy(1),
            Message::PackUsedBy(2),
            Message::PackUseClip { track: 0, clip: 1 },
            Message::PackUseTransition { track: 0, after_clip: 1 },
            Message::PackRemoved(name()),
            Message::PackRemoveFailed(name()),
            Message::PackToggleFailed(name()),
            Message::PackOpenFolderFailed(name()),
            Message::PackSaveWithPacksFailed(name()),
            Message::PackBackupFailed(name()),
            Message::PackRestoreOffer(1),
            Message::PackRestoreOffer(3),
            Message::PackRestoreInstall,
            Message::PackRestoreDismiss,
            Message::PackRestored(1),
            Message::PackRestored(2),
            Message::PackRestoreFailed(name()),
            Message::NoUserPackageFolder,
            Message::PackTabName(crate::package_manager::PackTab::Effect),
            Message::PackTabName(crate::package_manager::PackTab::Transition),
            Message::PackTabName(crate::package_manager::PackTab::TextStyle),
            Message::PackTabName(crate::package_manager::PackTab::SubtitleStyle),
            Message::PackTabName(crate::package_manager::PackTab::ClipAnimation),
            Message::PackTabName(crate::package_manager::PackTab::Sticker),
            Message::PackTabName(crate::package_manager::PackTab::Sfx),
            Message::PackTabName(crate::package_manager::PackTab::Music),
            Message::PackTabName(crate::package_manager::PackTab::Font),
            Message::AutosaveOff,
            Message::AutosaveEvery(30),
            Message::AutosaveEvery(60),
            Message::AutosaveEvery(300),
            Message::AutosaveSettingFailed(name()),
            Message::AutosaveFailed(name()),
            Message::MenuProxy,
            Message::ProxyOff,
            Message::ProxyAuto,
            Message::ProxyOn,
            Message::ProxyCancel,
            Message::ProxyRetry,
            Message::ProxyMaking { left: 2, percent: 40 },
            Message::ProxyBadge,
            Message::ProxyQueued,
            Message::ProxyRunning(40),
            Message::ProxyReady,
            Message::ProxyFailed(name()),
            Message::ProxyCancelled,
            Message::ProxySettingFailed(name()),
            Message::RecoveryTitle,
            Message::RecoveryIntro,
            Message::RecoveryRestore,
            Message::RecoveryDiscard,
            Message::RecoveryLater,
            Message::RecoveryAgo(0),
            Message::RecoveryAgo(90),
            Message::RecoveryAgo(5000),
            Message::RecoveryAgo(900_000),
            Message::RecoveryOriginalNewer,
            Message::RecoveryRestored(String::new()),
            Message::RecoveryRestored(name()),
            Message::RecoveryFailed(name()),
            Message::RecoveryBusy,
            Message::RecoveryTaken(name()),
            Message::RecoveryKeptDays(14),
            Message::TabMedia,
            Message::FieldDuration,
            Message::FieldSourceIn,
            Message::FieldVideoDuration,
            Message::FieldPlacement,
            Message::FieldOpacity,
            Message::FieldRotation,
            Message::FieldPositionX,
            Message::FieldPositionY,
            Message::FieldWidth,
            Message::FieldEasing,
            Message::KeyframeAdd,
            Message::KeyframeRemove,
            Message::KeyframePrevious,
            Message::KeyframeNext,
            Message::EasingLinear,
            Message::EasingEaseIn,
            Message::EasingEaseOut,
            Message::EasingEaseInOut,
            Message::FieldSpeed,
            Message::AddTrack,
            Message::EditTrackLocked(TrackLabel::Video(1)),
            Message::EditSpeedOverlap { track: TrackLabel::Video(1), end: "8.00".to_owned(), next: "4.00".to_owned() },
            Message::TrackLockedNote,
            Message::SpeedBlockedByEndTransition,
            Message::LockTrack,
            Message::UnlockTrack,
            Message::HideTrack,
            Message::DeleteTrack,
            Message::ShowTrack,
            Message::FieldBeats,
            Message::DetectBeats,
            Message::DetectBeatsHint,
            Message::ClearBeats,
            Message::CancelBeats,
            Message::BeatsDetecting(42),
            Message::BeatsSummary { count: 12, tempo: "120".to_owned() },
            Message::BeatsNone,
            Message::BeatsFound { count: 12, tempo: "120".to_owned() },
            Message::BeatsNotFound,
            Message::BeatsKeptForRedo,
            Message::BeatsFailed("x".to_owned()),
            Message::MarkerAddShortcut,
            Message::MarkerHint,
            Message::MarkerRemove,
            Message::MarkerLabel,
            Message::MarkerColor,
            Message::MarkerDefaultColor,
        ];
        for language in Language::ALL {
            for message in &all {
                assert!(!language.text(message).trim().is_empty(), "{language:?}: {message:?}");
            }
        }
    }

    #[test]
    fn english_counts_and_articles_read_right() {
        let finished = |frames| Language::English.t(Message::ExportFinished { frames, path: "a.mp4".to_owned(), encoder: "x".to_owned() });
        assert!(finished(1).contains("Exported 1 frame to"), "{}", finished(1));
        assert!(finished(2).contains("Exported 2 frames to"), "{}", finished(2));
        assert!(finished(0).contains("Exported 0 frames to"), "{}", finished(0));
        assert!(Language::English.t(Message::ExportFinishedSilent { frames: 1, path: "a.mp4".to_owned(), encoder: "x".to_owned() }).contains("Exported 1 frame to"));
        assert_eq!(Language::English.t(Message::StickerPackProblems(1)), "1 pack or sticker could not be read");
        assert_eq!(Language::Vietnamese.t(Message::StickerPackProblems(2)), "Không đọc được 2 gói hoặc sticker");
        assert_eq!(Language::Vietnamese.t(Message::ReadingStickerPacks), "Đang đọc các gói sticker đã cài...");
        assert_eq!(Language::English.t(Message::PackageProblems(1)), "1 package could not be read");
        assert_eq!(Language::English.t(Message::PackageProblems(3)), "3 packages could not be read");
        let unusable = |stream| Language::English.t(Message::StreamUnusable { name: "a".to_owned(), stream, reason: "r".to_owned() });
        assert_eq!(unusable(StreamKind::Audio), "a has an audio stream it cannot use: r");
        assert_eq!(unusable(StreamKind::Video), "a has a video stream it cannot use: r");
    }

    #[test]
    fn vietnamese_split_is_told_apart_from_trim() {
        let vietnamese = Language::Vietnamese;
        assert!(vietnamese.t(Message::SplitShortcut).contains("Tách"));
        assert!(vietnamese.t(Message::SelectClip(ClipAction::Split)).contains("tách"));
        assert!(vietnamese.t(Message::TrimBefore).contains("Cắt"));
        assert_eq!(vietnamese.t(Message::AudioGapLine(5)), "Mất tiếng: 5 ms");
    }

    #[test]
    fn a_text_clip_is_not_called_an_image_and_presets_have_names() {
        let vietnamese = Language::Vietnamese.t(Message::EditSpeedOfText(TrackLabel::Video(1)));
        assert!(vietnamese.contains("chữ") && !vietnamese.contains("ảnh"), "{vietnamese}");
        let english = Language::English.t(Message::EditSpeedOfText(TrackLabel::Video(1)));
        assert!(english.contains("text") && !english.contains("image"), "{english}");
        assert_eq!(Language::English.t(Message::AnimationPreset("slide-left")), "Slide left");
        assert_eq!(Language::Vietnamese.t(Message::AnimationPreset("fade")), "Mờ dần");
        assert_eq!(Language::English.t(Message::AnimationPreset("new-one")), "new-one", "an unknown id shows as it is");
        for preset in engine::ANIMATION_PRESETS {
            assert_ne!(Language::Vietnamese.t(Message::AnimationPreset(preset.id)), preset.id, "{} has a Vietnamese name", preset.id);
        }
    }

    #[test]
    fn technical_text_is_passed_through_as_it_is() {
        for language in Language::ALL {
            assert_eq!(language.t(Message::Technical("avcodec failed".to_owned())), "avcodec failed");
        }
    }

    #[test]
    fn the_two_languages_differ_and_parameters_arrive() {
        assert_ne!(Language::English.t(Message::MenuFile), Language::Vietnamese.t(Message::MenuFile));
        for language in Language::ALL {
            assert!(language.t(Message::Added("clip.mp4".to_owned())).contains("clip.mp4"));
            assert!(language.t(Message::ExportProgress { done: 3, total: 9 }).contains('9'));
        }
    }
}
