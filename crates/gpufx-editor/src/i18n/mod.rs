//! Every text the editor shows. A [`Message`] says what to tell the user; [`Message::to_text`] maps
//! it to a key of the language files and its arguments in one exhaustive `match`, so a new message
//! does not compile until it has a key. The words live in `i18n/<code>.json` (see [`catalog`]):
//! English is built into the binary and is the last fallback, other languages ship in the `i18n`
//! folder beside the app, and the user can add or override files in `%LOCALAPPDATA%\gpufx-editor\i18n`.
//! Detail from an encoder, FFmpeg or a media probe travels as [`Message::Technical`] or as the
//! argument of a translated prefix, in English. A refused edit has a message of its own for each
//! reason.

pub mod catalog;
pub mod pack;
pub mod text;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::package_import::{EntryProblem, ImportProblem};
use crate::package_manager::PackTab;
use crate::packages::PackageKind;
use catalog::Catalog;
pub use text::Text;

/// The English file, built in so that every key has a text even with no folder beside the app.
pub const EMBEDDED_ENGLISH: &str = include_str!("../../i18n/en.json");
/// The language files of a checkout, used by a debug build or a test when there is no `i18n` folder
/// beside the executable.
const DEV_LANGUAGE_FOLDER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/i18n");
/// The folder of language files, beside the app and under the user's root.
pub const LANGUAGE_FOLDER: &str = "i18n";

/// The folder of language files that come with the app: beside the executable, else, in a debug
/// build or a test only, the checkout's. A release build never reads the folder it was built from.
fn shipped_language_folder() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let dev = (cfg!(debug_assertions) || cfg!(test)).then(|| PathBuf::from(DEV_LANGUAGE_FOLDER));
    [exe_dir.map(|dir| dir.join(LANGUAGE_FOLDER)), dev].into_iter().flatten().find(|folder| folder.is_dir())
}

/// `%LOCALAPPDATA%\gpufx-editor\i18n`, where the user adds or overrides language files. Tests leave
/// it out, so the files of the machine running them cannot change what they see.
fn user_language_folder() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    crate::project_file::user_package_root().map(|root| root.join(LANGUAGE_FOLDER))
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// The languages of this run, read once on first use.
pub fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        let folders: Vec<PathBuf> = shipped_language_folder().into_iter().chain(user_language_folder()).collect();
        let catalog = Catalog::load(EMBEDDED_ENGLISH, &folders);
        for problem in catalog.problems() {
            eprintln!("gpufx-editor: language file skipped: {problem}");
        }
        catalog
    })
}

/// A display language of the editor: English, or one found in a language folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Language(usize);

impl Language {
    /// Always there, as it is built in.
    pub const ENGLISH: Self = Self(0);

    /// Every language found: English first, then by code.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..catalog().len()).map(Self)
    }

    /// How the settings file names the language: the name of its file.
    pub fn code(self) -> &'static str {
        catalog().code(self.0)
    }

    /// The language with settings code `code`, if a file of it was found.
    pub fn from_code(code: &str) -> Option<Self> {
        catalog().find(code).map(Self)
    }

    /// The name of the language in the language itself, from its file's `_meta.name`.
    pub fn native_name(self) -> &'static str {
        catalog().name(self.0)
    }

    pub fn text(self, message: &Message) -> String {
        catalog().resolve(self.0, &message.to_text())
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
    /// A tracker run wrote `count` samples and reached its end.
    TrackingDone { count: usize },
    /// A tracker run lost the object after `count` samples; the last one is marked to fix by hand.
    TrackingLost { count: usize },
    TrackingCancelled { count: usize },
    TrackingFailed(String),
    /// The motion track changed while a run went (another file, or its start box moved or went),
    /// so nothing of the run was written.
    TrackingStale,
    TrackCreated,
    FollowNoReference,
    FollowBaked,
    FieldTracking,
    TrackPick,
    TrackPickHint,
    TrackPicking,
    TrackForward,
    TrackBackward,
    TrackOneFrame,
    TrackStop,
    TrackDelete,
    TrackNone,
    TrackName { id: u32, count: usize },
    TrackRunning(u32),
    TrackSmooth,
    TrackHintFix,
    TrackMode,
    TrackModeFast,
    TrackModeFastHint,
    TrackModeRobust,
    TrackModeRobustHint,
    TrackModeRobustPlanar,
    TrackModelMissing,
    TrackModelInstalled,
    /// The face detector model of the automatic face censor was installed.
    FaceModelInstalled,
    /// The face detector model is not installed.
    FaceModelMissing,
    /// The button that hides every face of a video clip.
    FaceCensor,
    FaceCensorHint,
    /// Finding faces, percent done.
    FaceCensorRunning(u32),
    FaceCensorDone { count: usize },
    FaceCensorNone,
    FaceCensorCancelled,
    FaceCensorStale,
    FaceCensorFailed(String),
    FilterTrackingModel,
    AddCensorFollowing,
    FollowTrack,
    FollowScale,
    FollowRotation,
    /// Pin the overlay's corners to the object's four corners in perspective (corner pin).
    FollowPerspective,
    FollowPerspectiveHint,
    FollowBakePerspective,
    /// The track type: four corners in perspective rather than a box.
    TrackPlanar,
    TrackPlanarHint,
    FollowClear,
    FollowBake,
    FollowingTrack(u32),
    /// Lock the frame of a video clip to the tracked object, so the object stays put on screen.
    LockToObject,
    LockedToTrack(u32),
    LockScale,
    LockRotation,
    LockPerspectiveHint,
    LockPerspectiveNeedsPlanar,
    LockZoom,
    LockClear,
    LockBaked,
    EffectRegion,
    RegionCenterX,
    RegionCenterY,
    RegionWidth,
    RegionHeight,
    RegionLinkNone,
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

impl Message {
    /// The key of this message in the language files, and its arguments.
    pub fn to_text(&self) -> Text {
        match self {
        Message::Ready => Text::key("ready"),
        Message::Loading(name) => Text::key("loading").arg("name", name),
        Message::RefusedLine(detail) => Text::key("refusedLine").arg("detail", detail),
        Message::PreviewLine(detail) => Text::key("previewLine").arg("detail", detail),
        Message::AudioLine(detail) => Text::key("audioLine").arg("detail", detail),
        Message::NoSoundLine(detail) => Text::key("noSoundLine").arg("detail", detail),
        Message::AudioGapLine(ms) => Text::key("audioGapLine").arg("ms", ms),
        Message::FinishDragFirst => Text::key("finishDragFirst"),
        Message::ExportRunning => Text::key("exportRunning"),
        Message::ExportAlreadyRunning => Text::key("exportAlreadyRunning"),
        Message::NoNeighbour => Text::key("noNeighbour"),
        Message::NothingAtPlayhead => Text::key("nothingAtPlayhead"),
        Message::NothingToPlay => Text::key("nothingToPlay"),
        Message::NothingToUndo => Text::key("nothingToUndo"),
        Message::NothingToRedo => Text::key("nothingToRedo"),
        Message::Saved(path) => Text::key("saved").arg("path", path),
        Message::Opened(path) => Text::key("opened").arg("path", path),
        Message::Exporting => Text::key("exporting"),
        Message::Added(name) => Text::key("added").arg("name", name),
        Message::AddedWithLength { name, seconds } => Text::key("addedWithLength").arg("name", name).arg("seconds", seconds),
        Message::AddedNoLength { name, seconds } => Text::key("addedNoLength").arg("name", name).arg("seconds", seconds),
        Message::Imported(name) => Text::key("imported").arg("name", name),
        Message::NoUsableStream(name) => Text::key("noUsableStream").arg("name", name),
        Message::ExpressionDropped => Text::key("expressionDropped"),
        Message::LanguageSaveFailed(detail) => Text::key("languageSaveFailed").arg("detail", detail),
        Message::PanelSizesSaveFailed(detail) => Text::key("panelSizesSaveFailed").arg("detail", detail),
        Message::ResizePanelHint => Text::key("resizePanelHint"),
        Message::EditTrackNotFound(number) => Text::key("editTrackNotFound").arg("number", number),
        Message::EditInvalid(detail) => Text::key("editInvalid").arg("detail", detail),
        Message::DialogOpen => Text::key("dialogOpen"),
        Message::LoadRunning => Text::key("loadRunning"),
        Message::LoadDropped => Text::key("loadDropped"),
        Message::OpenCancelled => Text::key("openCancelled"),
        Message::OpenFailed(detail) => Text::key("openFailed").arg("detail", detail),
        Message::SaveFailed(detail) => Text::key("saveFailed").arg("detail", detail),
        Message::TaskFailed(detail) => Text::key("taskFailed").arg("detail", detail),
        Message::PromptDiscardForNew => Text::key("promptDiscardForNew"),
        Message::PromptDiscardForOpen => Text::key("promptDiscardForOpen"),
        Message::PromptDiscardForClose => Text::key("promptDiscardForClose"),
        Message::PromptCancelExport => Text::key("promptCancelExport"),
        Message::NothingToExport => Text::key("nothingToExport"),
        Message::ExportProgress { done, total } => Text::key("exportProgress").arg("done", done).arg("total", total),
        Message::ExportCancelled => Text::key("exportCancelled"),
        Message::ExportFailed(detail) => Text::key("exportFailed").arg("detail", detail),
        Message::ExportToolMissing(path) => Text::key("exportToolMissing").arg("path", path),
        Message::ExportResultUnknown(files) => Text::key("exportResultUnknown").arg("files", files),
        Message::ExportRendererMismatch => Text::key("exportRendererMismatch"),
        Message::EncoderAuto => Text::key("encoderAuto"),
        Message::EncoderNeedsGpl(name) => Text::key("encoderNeedsGpl").arg("name", name),
        Message::EncoderMissing(name) => Text::key("encoderMissing").arg("name", name),
        Message::EncoderProbing(name) => Text::key("encoderProbing").arg("name", name),
        Message::EncoderUnavailable(name) => Text::key("encoderUnavailable").arg("name", name),
        Message::FilterMedia => Text::key("filterMedia"),
        Message::FilterAudio => Text::key("filterAudio"),
        Message::FilterProject => Text::key("filterProject"),
        Message::FilterAllFiles => Text::key("filterAllFiles"),
        Message::Dirty => Text::key("dirty"),
        Message::Untitled => Text::key("untitled"),
        Message::MenuFile => Text::key("menuFile"),
        Message::MenuNewProject => Text::key("menuNewProject"),
        Message::MenuOpen => Text::key("menuOpen"),
        Message::MenuSave => Text::key("menuSave"),
        Message::MenuSaveAs => Text::key("menuSaveAs"),
        Message::MenuAgentAccess => Text::key("menuAgentAccess"),
        Message::AgentConnected => Text::key("agentConnected"),
        Message::ProjectInUse(path) => Text::key("projectInUse").arg("path", path),
        Message::AgentAccessFailed(detail) => Text::key("agentAccessFailed").arg("detail", detail),
        Message::MenuPackageManager => Text::key("menuPackageManager"),
        Message::MenuSaveWithPacks => Text::key("menuSaveWithPacks"),
        Message::PackManagerTitle => Text::key("packManagerTitle"),
        Message::PackColumnName => Text::key("packColumnName"),
        Message::PackColumnVersion => Text::key("packColumnVersion"),
        Message::PackColumnSource => Text::key("packColumnSource"),
        Message::PackColumnItems => Text::key("packColumnItems"),
        Message::PackColumnSize => Text::key("packColumnSize"),
        Message::PackColumnEnabled => Text::key("packColumnEnabled"),
        Message::PackSourceShipped => Text::key("packSourceShipped"),
        Message::PackSourceUser => Text::key("packSourceUser"),
        Message::PackInternal => Text::key("packInternal"),
        Message::PackShadowed => Text::key("packShadowed"),
        Message::PackReadOnly => Text::key("packReadOnly"),
        Message::PackBusyExporting => Text::key("packBusyExporting"),
        Message::PackInstallZip => Text::key("packInstallZip"),
        Message::PackRemove => Text::key("packRemove"),
        Message::PackOpenFolder => Text::key("packOpenFolder"),
        Message::PackRefresh => Text::key("packRefresh"),
        Message::PackFilter => Text::key("packFilter"),
        Message::PackNone => Text::key("packNone"),
        Message::PackRemoveQuestion(name) => Text::key("packRemoveQuestion").arg("name", name),
        Message::PackRemoved(name) => Text::key("packRemoved").arg("name", name),
        Message::PackRemoveFailed(detail) => Text::key("packRemoveFailed").arg("detail", detail),
        Message::PackToggleFailed(detail) => Text::key("packToggleFailed").arg("detail", detail),
        Message::PackOpenFolderFailed(detail) => Text::key("packOpenFolderFailed").arg("detail", detail),
        Message::PackSaveWithPacksFailed(detail) => Text::key("packSaveWithPacksFailed").arg("detail", detail),
        Message::PackBackupFailed(detail) => Text::key("packBackupFailed").arg("detail", detail),
        Message::PackRestoreInstall => Text::key("packRestoreInstall"),
        Message::PackRestoreDismiss => Text::key("packRestoreDismiss"),
        Message::PackRestoreFailed(detail) => Text::key("packRestoreFailed").arg("detail", detail),
        Message::NoUserPackageFolder => Text::key("noUserPackageFolder"),
        Message::MenuAutosave => Text::key("menuAutosave"),
        Message::AutosaveOff => Text::key("autosaveOff"),
        Message::AutosaveSettingFailed(detail) => Text::key("autosaveSettingFailed").arg("detail", detail),
        Message::AutosaveFailed(detail) => Text::key("autosaveFailed").arg("detail", detail),
        Message::MenuProxy => Text::key("menuProxy"),
        Message::ProxyOff => Text::key("proxyOff"),
        Message::ProxyAuto => Text::key("proxyAuto"),
        Message::ProxyOn => Text::key("proxyOn"),
        Message::ProxyCancel => Text::key("proxyCancel"),
        Message::ProxyRetry => Text::key("proxyRetry"),
        Message::ProxyMaking { left, percent } => Text::key("proxyMaking").arg("left", left).arg("percent", percent),
        Message::ProxyBadge => Text::key("proxyBadge"),
        Message::ProxyQueued => Text::key("proxyQueued"),
        Message::ProxyRunning(percent) => Text::key("proxyRunning").arg("percent", percent),
        Message::ProxyReady => Text::key("proxyReady"),
        Message::ProxyFailed(detail) => Text::key("proxyFailed").arg("detail", detail),
        Message::ProxyCancelled => Text::key("proxyCancelled"),
        Message::ProxySettingFailed(detail) => Text::key("proxySettingFailed").arg("detail", detail),
        Message::RecoveryTitle => Text::key("recoveryTitle"),
        Message::RecoveryIntro => Text::key("recoveryIntro"),
        Message::RecoveryRestore => Text::key("recoveryRestore"),
        Message::RecoveryDiscard => Text::key("recoveryDiscard"),
        Message::RecoveryLater => Text::key("recoveryLater"),
        Message::RecoveryOriginalNewer => Text::key("recoveryOriginalNewer"),
        Message::RecoveryFailed(detail) => Text::key("recoveryFailed").arg("detail", detail),
        Message::RecoveryBusy => Text::key("recoveryBusy"),
        Message::RecoveryTaken(detail) => Text::key("recoveryTaken").arg("detail", detail),
        Message::RecoveryKeptDays(days) => Text::key("recoveryKeptDays").arg("days", days),
        Message::MenuLanguage => Text::key("menuLanguage"),
        Message::MenuTheme => Text::key("menuTheme"),
        Message::ThemeSystem => Text::key("themeSystem"),
        Message::ThemeDark => Text::key("themeDark"),
        Message::ThemeLight => Text::key("themeLight"),
        Message::ThemeSaveFailed(detail) => Text::key("themeSaveFailed").arg("detail", detail),
        Message::Cancel => Text::key("cancel"),
        Message::ExportVideo => Text::key("exportVideo"),
        Message::TabMedia => Text::key("tabMedia"),
        Message::TabAudio => Text::key("tabAudio"),
        Message::TabStickers => Text::key("tabStickers"),
        Message::SearchStickers => Text::key("searchStickers"),
        Message::AllCategories => Text::key("allCategories"),
        Message::StickerLibraryEmpty => Text::key("stickerLibraryEmpty"),
        Message::StickerAddHint => Text::key("stickerAddHint"),
        Message::ReadingStickerPacks => Text::key("readingStickerPacks"),
        Message::EditMarkerNotFound(marker) => Text::key("editMarkerNotFound").arg("marker", marker),
        Message::TabText => Text::key("tabText"),
        Message::AddText => Text::key("addText"),
        Message::TextAddHint => Text::key("textAddHint"),
        Message::TextStyles => Text::key("textStyles"),
        Message::SubtitleStyles => Text::key("subtitleStyles"),
        Message::ReadingTextStylePacks => Text::key("readingTextStylePacks"),
        Message::TextStyleLibraryEmpty => Text::key("textStyleLibraryEmpty"),
        Message::ApplyTextStyle => Text::key("applyTextStyle"),
        Message::DefaultTextContent => Text::key("defaultTextContent"),
        Message::TextAdded => Text::key("textAdded"),
        Message::FieldTextContent => Text::key("fieldTextContent"),
        Message::FieldFont => Text::key("fieldFont"),
        Message::DefaultFont => Text::key("defaultFont"),
        Message::ChooseFontFile => Text::key("chooseFontFile"),
        Message::FilterFont => Text::key("filterFont"),
        Message::ImportSubtitles => Text::key("importSubtitles"),
        Message::FilterSubtitles => Text::key("filterSubtitles"),
        Message::SubtitlesNone => Text::key("subtitlesNone"),
        Message::SubtitleFileUnreadable(reason) => Text::key("subtitleFileUnreadable").arg("reason", reason),
        Message::SubtitleTrack => Text::key("subtitleTrack"),
        Message::SubtitleTrackStyle => Text::key("subtitleTrackStyle"),
        Message::SubtitleStyleNone => Text::key("subtitleStyleNone"),
        Message::SubtitleOwnStyle => Text::key("subtitleOwnStyle"),
        Message::Karaoke => Text::key("karaoke"),
        Message::KaraokeColor => Text::key("karaokeColor"),
        Message::KaraokeBox => Text::key("karaokeBox"),
        Message::KaraokeScale => Text::key("karaokeScale"),
        Message::KaraokeActiveColor => Text::key("karaokeActiveColor"),
        Message::ExportSubtitles => Text::key("exportSubtitles"),
        Message::SubtitlesExported(path) => Text::key("subtitlesExported").arg("path", path),
        Message::SubtitleExportTargetChanged => Text::key("subtitleExportTargetChanged"),
        Message::SubtitleExportFailed(reason) => Text::key("subtitleExportFailed").arg("reason", reason),
        Message::FontTargetChanged => Text::key("fontTargetChanged"),
        Message::LoadingFonts => Text::key("loadingFonts"),
        Message::SearchFonts => Text::key("searchFonts"),
        Message::FontSourceAll => Text::key("fontSourceAll"),
        Message::FontSourceSystem => Text::key("fontSourceSystem"),
        Message::FontSourcePack => Text::key("fontSourcePack"),
        Message::NoFontFound => Text::key("noFontFound"),
        Message::FontMissing(family) => Text::key("fontMissing").arg("family", family),
        Message::TextureMissing(picture) => Text::key("textureMissing").arg("picture", picture),
        Message::FieldTextSize => Text::key("fieldTextSize"),
        Message::FieldTextColor => Text::key("fieldTextColor"),
        Message::TextOutline => Text::key("textOutline"),
        Message::TextBackground => Text::key("textBackground"),
        Message::TextShadow => Text::key("textShadow"),
        Message::TextGlow => Text::key("textGlow"),
        Message::FieldOuterColor => Text::key("fieldOuterColor"),
        Message::FieldGlowRadius => Text::key("fieldGlowRadius"),
        Message::FieldIntensity => Text::key("fieldIntensity"),
        Message::FieldFalloff => Text::key("fieldFalloff"),
        Message::FieldColor => Text::key("fieldColor"),
        Message::FieldThickness => Text::key("fieldThickness"),
        Message::FieldPadding => Text::key("fieldPadding"),
        Message::FieldCornerRadius => Text::key("fieldCornerRadius"),
        Message::FieldShadowOffset => Text::key("fieldShadowOffset"),
        Message::FieldBlur => Text::key("fieldBlur"),
        Message::FieldTextureMix => Text::key("fieldTextureMix"),
        Message::LayerLimit(count) => Text::key("layerLimit").arg("count", count),
        Message::TextLayers => Text::key("textLayers"),
        Message::TextFill => Text::key("textFill"),
        Message::PaintSolid => Text::key("paintSolid"),
        Message::PaintGradient => Text::key("paintGradient"),
        Message::PaintTexture => Text::key("paintTexture"),
        Message::FieldAngle => Text::key("fieldAngle"),
        Message::AddGradientStop => Text::key("addGradientStop"),
        Message::FieldPosition => Text::key("fieldPosition"),
        Message::TexturePicture => Text::key("texturePicture"),
        Message::ChooseTextureFile => Text::key("chooseTextureFile"),
        Message::FitStretch => Text::key("fitStretch"),
        Message::FitTile => Text::key("fitTile"),
        Message::FitMirror => Text::key("fitMirror"),
        Message::FitGlyph => Text::key("fitGlyph"),
        Message::FieldCells => Text::key("fieldCells"),
        Message::FlipX => Text::key("flipX"),
        Message::FlipY => Text::key("flipY"),
        Message::TextureBase => Text::key("textureBase"),
        Message::BaseNone => Text::key("baseNone"),
        Message::BaseSolid => Text::key("baseSolid"),
        Message::BaseGradient => Text::key("baseGradient"),
        Message::GradientPerGlyph => Text::key("gradientPerGlyph"),
        Message::FieldScale => Text::key("fieldScale"),
        Message::TextOutlines => Text::key("textOutlines"),
        Message::TextShadows => Text::key("textShadows"),
        Message::TextInnerShadows => Text::key("textInnerShadows"),
        Message::TextInnerShadow => Text::key("textInnerShadow"),
        Message::TextBloom => Text::key("textBloom"),
        Message::AddLayer => Text::key("addLayer"),
        Message::MoveLayerUp => Text::key("moveLayerUp"),
        Message::MoveLayerDown => Text::key("moveLayerDown"),
        Message::FieldDistance => Text::key("fieldDistance"),
        Message::FieldStrength => Text::key("fieldStrength"),
        Message::FieldSpread => Text::key("fieldSpread"),
        Message::FieldShiftX => Text::key("fieldShiftX"),
        Message::FieldShiftY => Text::key("fieldShiftY"),
        Message::ShadowOutlines => Text::key("shadowOutlines"),
        Message::FilterPicture => Text::key("filterPicture"),
        Message::TextureTargetChanged => Text::key("textureTargetChanged"),
        Message::FieldAlign => Text::key("fieldAlign"),
        Message::AlignLeft => Text::key("alignLeft"),
        Message::AlignCenter => Text::key("alignCenter"),
        Message::AlignRight => Text::key("alignRight"),
        Message::FieldLineSpacing => Text::key("fieldLineSpacing"),
        Message::FieldFontWeight => Text::key("fieldFontWeight"),
        Message::TextItalic => Text::key("textItalic"),
        Message::FieldTextTransform => Text::key("fieldTextTransform"),
        Message::TransformNone => Text::key("transformNone"),
        Message::TransformUppercase => Text::key("transformUppercase"),
        Message::TransformLowercase => Text::key("transformLowercase"),
        Message::FieldLetterSpacing => Text::key("fieldLetterSpacing"),
        Message::TabAnimation => Text::key("tabAnimation"),
        Message::ClipTabProperties => Text::key("clipTabProperties"),
        Message::ClipTabPicture => Text::key("clipTabPicture"),
        Message::ClipTabGeneral => Text::key("clipTabGeneral"),
        Message::HoldRemovedTransition => Text::key("holdRemovedTransition"),
        Message::TransitionReleasedHold => Text::key("transitionReleasedHold"),
        Message::AnimationHoldNote(seconds) => Text::key("animationHoldNote").arg("seconds", seconds),
        Message::AnimationDurationHint => Text::key("animationDurationHint"),
        Message::AnimationEnter => Text::key("animationEnter"),
        Message::AnimationExit => Text::key("animationExit"),
        Message::AnimationRepeat => Text::key("animationRepeat"),
        Message::AnimationNone => Text::key("animationNone"),
        Message::AnimationPackMissing(id) => Text::key("animationPackMissing").arg("id", id),
        Message::FieldAnimationDuration => Text::key("fieldAnimationDuration"),
        Message::TabEffects => Text::key("tabEffects"),
        Message::Transitions => Text::key("transitions"),
        Message::ImportFromComputer => Text::key("importFromComputer"),
        Message::ImportPackage => Text::key("importPackage"),
        Message::FilterPackage => Text::key("filterPackage"),
        Message::PromptOverwritePackage(name) => Text::key("promptOverwritePackage").arg("name", name),
        Message::SessionAudio => Text::key("sessionAudio"),
        Message::InstalledPacks => Text::key("installedPacks"),
        Message::ReadingAudioPacks => Text::key("readingAudioPacks"),
        Message::AudioLibraryEmpty => Text::key("audioLibraryEmpty"),
        Message::PreviewSound => Text::key("previewSound"),
        Message::StopPreview => Text::key("stopPreview"),
        Message::AddSound => Text::key("addSound"),
        Message::AudioAddHint => Text::key("audioAddHint"),
        Message::SoundPreviewFailed(reason) => Text::key("soundPreviewFailed").arg("reason", reason),
        Message::SearchMedia => Text::key("searchMedia"),
        Message::SearchAudio => Text::key("searchAudio"),
        Message::SearchEffects => Text::key("searchEffects"),
        Message::LibraryEmpty => Text::key("libraryEmpty"),
        Message::NoSearchMatch => Text::key("noSearchMatch"),
        Message::AddHint => Text::key("addHint"),
        Message::ReadingPackages => Text::key("readingPackages"),
        Message::NoPackages(variable) => Text::key("noPackages").arg("variable", variable),
        Message::AddToClip => Text::key("addToClip"),
        Message::PreviewFailed(reason) => Text::key("previewFailed").arg("reason", reason),
        Message::PreviewUnavailable(reason) => Text::key("previewUnavailable").arg("reason", reason),
        Message::ClipTitleVideo => Text::key("clipTitleVideo"),
        Message::ClipTitleAudio => Text::key("clipTitleAudio"),
        Message::ClipTitleImage => Text::key("clipTitleImage"),
        Message::ClipTitleSticker => Text::key("clipTitleSticker"),
        Message::ClipTitleText => Text::key("clipTitleText"),
        Message::TransitionTitle => Text::key("transitionTitle"),
        Message::ProjectTitle => Text::key("projectTitle"),
        Message::FieldSize => Text::key("fieldSize"),
        Message::FieldFrameRate => Text::key("fieldFrameRate"),
        Message::FieldAspect => Text::key("fieldAspect"),
        Message::FieldDuration => Text::key("fieldDuration"),
        Message::FieldSpeed => Text::key("fieldSpeed"),
        Message::FieldStart => Text::key("fieldStart"),
        Message::DropOnTimeline => Text::key("dropOnTimeline"),
        Message::OutputSizeCustom => Text::key("outputSizeCustom"),
        Message::FrameRateCustom => Text::key("frameRateCustom"),
        Message::Apply => Text::key("apply"),
        Message::FieldSourceIn => Text::key("fieldSourceIn"),
        Message::FieldVideoDuration => Text::key("fieldVideoDuration"),
        Message::FieldPlacement => Text::key("fieldPlacement"),
        Message::FieldOpacity => Text::key("fieldOpacity"),
        Message::FieldRotation => Text::key("fieldRotation"),
        Message::FieldPositionX => Text::key("fieldPositionX"),
        Message::FieldPositionY => Text::key("fieldPositionY"),
        Message::FieldWidth => Text::key("fieldWidth"),
        Message::FieldEasing => Text::key("fieldEasing"),
        Message::KeyframeAdd => Text::key("keyframeAdd"),
        Message::KeyframeRemove => Text::key("keyframeRemove"),
        Message::KeyframePrevious => Text::key("keyframePrevious"),
        Message::KeyframeNext => Text::key("keyframeNext"),
        Message::EasingLinear => Text::key("easingLinear"),
        Message::EasingEaseIn => Text::key("easingEaseIn"),
        Message::EasingEaseOut => Text::key("easingEaseOut"),
        Message::EasingEaseInOut => Text::key("easingEaseInOut"),
        Message::FieldVolume => Text::key("fieldVolume"),
        Message::SelectClipHint => Text::key("selectClipHint"),
        Message::Mute => Text::key("mute"),
        Message::Reverse => Text::key("reverse"),
        Message::FadeIn => Text::key("fadeIn"),
        Message::FadeOut => Text::key("fadeOut"),
        Message::FieldDenoise => Text::key("fieldDenoise"),
        Message::FieldMatte => Text::key("fieldMatte"),
        Message::MatteOff => Text::key("matteOff"),
        Message::MatteQuality => Text::key("matteQuality"),
        Message::MatteQualityHint => Text::key("matteQualityHint"),
        Message::MatteFast => Text::key("matteFast"),
        Message::MatteFastHint => Text::key("matteFastHint"),
        Message::MatteModelMissing => Text::key("matteModelMissing"),
        Message::MatteImportZip => Text::key("matteImportZip"),
        Message::MatteFetch => Text::key("matteFetch"),
        Message::MatteFetchUnavailable => Text::key("matteFetchUnavailable"),
        Message::MatteFetching => Text::key("matteFetching"),
        Message::MatteFetchCancel => Text::key("matteFetchCancel"),
        Message::MatteFetchRetry => Text::key("matteFetchRetry"),
        Message::MatteThreshold => Text::key("matteThreshold"),
        Message::MatteFeather => Text::key("matteFeather"),
        Message::MatteEdge => Text::key("matteEdge"),
        Message::AddCensor => Text::key("addCensor"),
        Message::AddCensorHint => Text::key("addCensorHint"),
        Message::CensorAdded => Text::key("censorAdded"),
        Message::ClipTitleCensor => Text::key("clipTitleCensor"),
        Message::CensorShape => Text::key("censorShape"),
        Message::CensorShapeRect => Text::key("censorShapeRect"),
        Message::CensorShapeEllipse => Text::key("censorShapeEllipse"),
        Message::CensorFeather => Text::key("censorFeather"),
        Message::CensorMode => Text::key("censorMode"),
        Message::CensorBlur => Text::key("censorBlur"),
        Message::CensorPixelate => Text::key("censorPixelate"),
        Message::CensorSolid => Text::key("censorSolid"),
        Message::CensorStrength => Text::key("censorStrength"),
        Message::CensorBlock => Text::key("censorBlock"),
        Message::CensorColor => Text::key("censorColor"),
        Message::MatteTemporal => Text::key("matteTemporal"),
        Message::MatteTemporalHint => Text::key("matteTemporalHint"),
        Message::MatteInvert => Text::key("matteInvert"),
        Message::MatteImported => Text::key("matteImported"),
        Message::MatteImportFailed(problem) => Text::key("matteImportFailed").arg("problem", problem),
        Message::FilterMatteModel => Text::key("filterMatteModel"),
        Message::DenoiseOff => Text::key("denoiseOff"),
        Message::DenoiseVoice => Text::key("denoiseVoice"),
        Message::DenoiseGeneral => Text::key("denoiseGeneral"),
        Message::DenoiseVoiceHint => Text::key("denoiseVoiceHint"),
        Message::DenoiseGeneralHint => Text::key("denoiseGeneralHint"),
        Message::DenoiseStrength => Text::key("denoiseStrength"),
        Message::FieldVoiceChange => Text::key("fieldVoiceChange"),
        Message::VoiceNormal => Text::key("voiceNormal"),
        Message::VoiceChipmunk => Text::key("voiceChipmunk"),
        Message::VoiceDeep => Text::key("voiceDeep"),
        Message::VoiceRobot => Text::key("voiceRobot"),
        Message::FieldPitch => Text::key("fieldPitch"),
        Message::FieldBeats => Text::key("fieldBeats"),
        Message::DetectBeats => Text::key("detectBeats"),
        Message::DetectBeatsHint => Text::key("detectBeatsHint"),
        Message::ClearBeats => Text::key("clearBeats"),
        Message::CancelBeats => Text::key("cancelBeats"),
        Message::BeatsDetecting(percent) => Text::key("beatsDetecting").arg("percent", percent),
        Message::BeatsNone => Text::key("beatsNone"),
        Message::BeatsNotFound => Text::key("beatsNotFound"),
        Message::BeatsKeptForRedo => Text::key("beatsKeptForRedo"),
        Message::BeatsFailed(reason) => Text::key("beatsFailed").arg("reason", reason),
        Message::TrackingFailed(reason) => Text::key("trackingFailed").arg("reason", reason),
        Message::TrackingStale => Text::key("trackingStale"),
        Message::TrackCreated => Text::key("trackCreated"),
        Message::FollowNoReference => Text::key("followNoReference"),
        Message::FollowBaked => Text::key("followBaked"),
        Message::FieldTracking => Text::key("fieldTracking"),
        Message::TrackPick => Text::key("trackPick"),
        Message::TrackPickHint => Text::key("trackPickHint"),
        Message::TrackPicking => Text::key("trackPicking"),
        Message::TrackForward => Text::key("trackForward"),
        Message::TrackBackward => Text::key("trackBackward"),
        Message::TrackOneFrame => Text::key("trackOneFrame"),
        Message::TrackStop => Text::key("trackStop"),
        Message::TrackDelete => Text::key("trackDelete"),
        Message::TrackNone => Text::key("trackNone"),
        Message::TrackRunning(value) => Text::key("trackRunning").arg("value", value),
        Message::TrackSmooth => Text::key("trackSmooth"),
        Message::TrackMode => Text::key("trackMode"),
        Message::TrackModeFast => Text::key("trackModeFast"),
        Message::TrackModeFastHint => Text::key("trackModeFastHint"),
        Message::TrackModeRobust => Text::key("trackModeRobust"),
        Message::TrackModeRobustPlanar => Text::key("trackModeRobustPlanar"),
        Message::TrackModeRobustHint => Text::key("trackModeRobustHint"),
        Message::TrackModelMissing => Text::key("trackModelMissing"),
        Message::TrackModelInstalled => Text::key("trackModelInstalled"),
        Message::FaceModelInstalled => Text::key("faceModelInstalled"),
        Message::FaceModelMissing => Text::key("faceModelMissing"),
        Message::FaceCensor => Text::key("faceCensor"),
        Message::FaceCensorHint => Text::key("faceCensorHint"),
        Message::FaceCensorRunning(value) => Text::key("faceCensorRunning").arg("value", value),
        Message::FaceCensorNone => Text::key("faceCensorNone"),
        Message::FaceCensorCancelled => Text::key("faceCensorCancelled"),
        Message::FaceCensorStale => Text::key("faceCensorStale"),
        Message::FaceCensorFailed(reason) => Text::key("faceCensorFailed").arg("reason", reason),
        Message::FilterTrackingModel => Text::key("filterTrackingModel"),
        Message::TrackHintFix => Text::key("trackHintFix"),
        Message::AddCensorFollowing => Text::key("addCensorFollowing"),
        Message::FollowTrack => Text::key("followTrack"),
        Message::FollowScale => Text::key("followScale"),
        Message::FollowRotation => Text::key("followRotation"),
        Message::FollowPerspective => Text::key("followPerspective"),
        Message::FollowBakePerspective => Text::key("followBakePerspective"),
        Message::FollowPerspectiveHint => Text::key("followPerspectiveHint"),
        Message::TrackPlanar => Text::key("trackPlanar"),
        Message::TrackPlanarHint => Text::key("trackPlanarHint"),
        Message::FollowClear => Text::key("followClear"),
        Message::FollowBake => Text::key("followBake"),
        Message::FollowingTrack(value) => Text::key("followingTrack").arg("value", value),
        Message::LockToObject => Text::key("lockToObject"),
        Message::LockedToTrack(value) => Text::key("lockedToTrack").arg("value", value),
        Message::LockScale => Text::key("lockScale"),
        Message::LockRotation => Text::key("lockRotation"),
        Message::LockPerspectiveHint => Text::key("lockPerspectiveHint"),
        Message::LockPerspectiveNeedsPlanar => Text::key("lockPerspectiveNeedsPlanar"),
        Message::LockZoom => Text::key("lockZoom"),
        Message::LockClear => Text::key("lockClear"),
        Message::LockBaked => Text::key("lockBaked"),
        Message::EffectRegion => Text::key("effectRegion"),
        Message::RegionCenterX => Text::key("regionCenterX"),
        Message::RegionCenterY => Text::key("regionCenterY"),
        Message::RegionWidth => Text::key("regionWidth"),
        Message::RegionHeight => Text::key("regionHeight"),
        Message::RegionLinkNone => Text::key("regionLinkNone"),
        Message::MarkerAddShortcut => Text::key("markerAddShortcut"),
        Message::MarkerHint => Text::key("markerHint"),
        Message::MarkerRemove => Text::key("markerRemove"),
        Message::MarkerLabel => Text::key("markerLabel"),
        Message::MarkerColor => Text::key("markerColor"),
        Message::MarkerDefaultColor => Text::key("markerDefaultColor"),
        Message::NoEffectsYet => Text::key("noEffectsYet"),
        Message::Remove => Text::key("remove"),
        Message::Reset => Text::key("reset"),
        Message::PackageParamsUnreadable => Text::key("packageParamsUnreadable"),
        Message::Variables(names) => Text::key("variables").arg("names", names),
        Message::Player => Text::key("player"),
        Message::Fullscreen => Text::key("fullscreen"),
        Message::ExitFullscreen => Text::key("exitFullscreen"),
        Message::PlayShortcut => Text::key("playShortcut"),
        Message::StopShortcut => Text::key("stopShortcut"),
        Message::UndoShortcut => Text::key("undoShortcut"),
        Message::RedoShortcut => Text::key("redoShortcut"),
        Message::SplitShortcut => Text::key("splitShortcut"),
        Message::TrimBefore => Text::key("trimBefore"),
        Message::TrimAfter => Text::key("trimAfter"),
        Message::SnapToggle => Text::key("snapToggle"),
        Message::DeleteShortcut => Text::key("deleteShortcut"),
        Message::ZoomIn => Text::key("zoomIn"),
        Message::ZoomOut => Text::key("zoomOut"),
        Message::AddTrack => Text::key("addTrack"),
        Message::AddVideoTrack => Text::key("addVideoTrack"),
        Message::AddAudioTrack => Text::key("addAudioTrack"),
        Message::VideoTrackNumber(number) => Text::key("videoTrackNumber").arg("number", number),
        Message::AudioTrackNumber(number) => Text::key("audioTrackNumber").arg("number", number),
        Message::AddStickerTrack => Text::key("addStickerTrack"),
        Message::AddTextTrack => Text::key("addTextTrack"),
        Message::StickerTrackNumber(number) => Text::key("stickerTrackNumber").arg("number", number),
        Message::TextTrackNumber(number) => Text::key("textTrackNumber").arg("number", number),
        Message::AddClipFromFile => Text::key("addClipFromFile"),
        Message::LockTrack => Text::key("lockTrack"),
        Message::UnlockTrack => Text::key("unlockTrack"),
        Message::DeleteTrack => Text::key("deleteTrack"),
        Message::HideTrack => Text::key("hideTrack"),
        Message::ShowTrack => Text::key("showTrack"),
        Message::TrackLockedNote => Text::key("trackLockedNote"),
        Message::SpeedBlockedByEndTransition => Text::key("speedBlockedByEndTransition"),
        Message::Technical(text) => Text::raw(text),
        Message::StreamUnusable { name, stream, reason } => Text::key("streamUnusable").arg("name", name).arg("stream", stream_with_article(*stream)).arg("reason", reason),
        Message::StreamMissing { name, stream } => Text::key("streamMissing").arg("name", name).arg("stream", stream_name(*stream)),
        Message::SelectClip(action) => select_clip(*action),
        Message::EditClipNotFound { track, clip } => Text::key("editClipNotFound").arg("track", track_label(*track)).arg("clip", clip),
        Message::EditEffectNotFound { track, clip, effect } => Text::key("editEffectNotFound").arg("track", track_label(*track)).arg("clip", clip).arg("effect", effect),
        Message::EditTransitionNotFound { track, after_clip } => Text::key("editTransitionNotFound").arg("track", track_label(*track)).arg("after_clip", after_clip),
        Message::EditTrackKindMismatch { from, to } => Text::key("editTrackKindMismatch").arg("from", track_kind_with_article(*from)).arg("to", track_kind_with_article(*to)),
        Message::EditNoRoom(track) => on_track("editNoRoom", *track),
        Message::EditOverlap { track, start, end } => on_track("editOverlap", *track).arg("start", start).arg("end", end),
        Message::EditTimeOverflow { track, clip } => on_track("editTimeOverflow", *track).arg("clip", clip),
        Message::EditTransitionOnEdge { track, edge } => on_track("editTransitionOnEdge", *track).arg("edge", clip_edge(*edge)),
        Message::EditBeforeSourceStart(track) => on_track("editBeforeSourceStart", *track),
        Message::EditTrackLocked(track) => on_track("editTrackLocked", *track),
        Message::EditSpeedOverlap { track, end, next } => on_track("editSpeedOverlap", *track).arg("end", end).arg("next", next),
        Message::EditSplitOutsideClip { track, at } => on_track("editSplitOutsideClip", *track).arg("at", at),
        Message::EditSplitInsideFade { track, at } => on_track("editSplitInsideFade", *track).arg("at", at),
        Message::EditSplitInsideTransition { track, at } => on_track("editSplitInsideTransition", *track).arg("at", at),
        Message::EditSplitInsideHold { track, at } => on_track("editSplitInsideHold", *track).arg("at", at),
        Message::EditSpeedOfImage(track) => on_track("editSpeedOfImage", *track),
        Message::EditKeyframeOutsideClip(track) => on_track("editKeyframeOutsideClip", *track),
        Message::EditKeyframeNotFound(track) => on_track("editKeyframeNotFound", *track),
        Message::EditNoPlacement(track) => on_track("editNoPlacement", *track),
        Message::EditSpeedOfText(track) => on_track("editSpeedOfText", *track),
        Message::EditReverseOfStill(track) => on_track("editReverseOfStill", *track),
        Message::ExportFinished { frames, path, encoder } => Text::key("exportFinished").count("frames", *frames).arg("path", path).arg("encoder", encoder),
        Message::ExportFinishedSilent { frames, path, encoder } => Text::key("exportFinishedSilent").count("frames", *frames).arg("path", path).arg("encoder", encoder),
        Message::ExportChildCrashed { code, stderr } if stderr.is_empty() => Text::key("exportChildCrashed").arg("code", code),
        Message::ExportChildCrashed { code, stderr } => Text::key("exportChildCrashedWithDetail").arg("code", code).arg("stderr", stderr),
        Message::PackTabName(tab) => pack_tab(*tab),
        Message::PackUsedBy(count) => Text::key("packUsedBy").count("count", *count as i64),
        Message::PackUseClip { track, clip } => Text::key("packUseClip").arg("track", track + 1).arg("clip", clip + 1),
        Message::PackUseTransition { track, after_clip } => Text::key("packUseTransition").arg("track", track + 1).arg("after_clip", after_clip + 1),
        Message::PackRestoreOffer(count) => Text::key("packRestoreOffer").count("count", *count as i64),
        Message::PackRestored(count) => Text::key("packRestored").count("count", *count as i64),
        Message::AutosaveEvery(seconds) if seconds % 60 == 0 => Text::key("autosaveEveryMinutes").count("count", (seconds / 60) as i64),
        Message::AutosaveEvery(seconds) => Text::key("autosaveEverySeconds").arg("seconds", seconds),
        Message::RecoveryAgo(seconds) => recovery_ago(*seconds),
        Message::RecoveryRestored(path) if path.is_empty() => Text::key("recoveryRestoredUntitled"),
        Message::RecoveryRestored(path) => Text::key("recoveryRestored").arg("path", path),
        Message::AnimatedSticker { loop_ms, loops } => {
            let seconds = format!("{:.2}", *loop_ms as f64 / 1000.0);
            if *loops == 0 { Text::key("animatedStickerForever").arg("seconds", seconds) } else { Text::key("animatedSticker").arg("seconds", seconds).arg("loops", loops) }
        }
        Message::StickerPackInstalled { name, replaced } => Text::key("stickerPackInstalled").arg("verb", install_verb(*replaced)).arg("name", name),
        Message::FontPackInstalled { name, replaced } => Text::key("fontPackInstalled").arg("verb", install_verb(*replaced)).arg("name", name),
        Message::TextStylePackInstalled { name, replaced } => Text::key("textStylePackInstalled").arg("verb", install_verb(*replaced)).arg("name", name),
        Message::ClipAnimationPackInstalled { name, replaced } => Text::key("clipAnimationPackInstalled").arg("verb", install_verb(*replaced)).arg("name", name),
        Message::AudioPackInstalled { name, replaced } => Text::key("audioPackInstalled").arg("verb", install_verb(*replaced)).arg("name", name),
        Message::PackageInstalled { name, transition, replaced } => Text::key("packageInstalled").arg("verb", install_verb(*replaced)).arg("name", name).arg("tab", Text::key(if *transition { "transitions" } else { "tabEffects" })),
        Message::StickerPackProblems(count) => Text::key("stickerPackProblems").count("count", *count as i64),
        Message::TextStylePackProblems(count) => Text::key("textStylePackProblems").count("count", *count as i64),
        Message::AudioPackProblems(count) => Text::key("audioPackProblems").count("count", *count as i64),
        Message::PackageProblems(count) => Text::key("packageProblems").count("count", *count as i64),
        Message::SubtitlesImported { cues, skipped: 0 } => Text::key("subtitlesImported").arg("cues", cue_count(*cues)),
        Message::SubtitlesImported { cues, skipped } => Text::key("subtitlesImportedSkipped").arg("cues", cue_count(*cues)).arg("skipped", cue_count(*skipped)),
        Message::BeatsSummary { count, tempo } => Text::key("beatsSummary").count("count", *count as i64).arg("tempo", tempo),
        Message::BeatsFound { count, tempo } => Text::key("beatsFound").count("count", *count as i64).arg("tempo", tempo),
        Message::TrackingDone { count } => Text::key("trackingDone").count("count", *count as i64),
        Message::TrackingLost { count } => Text::key("trackingLost").count("count", *count as i64),
        Message::TrackingCancelled { count } => Text::key("trackingCancelled").count("count", *count as i64),
        Message::TrackName { id, count } => Text::key("trackName").arg("id", id).count("count", *count as i64),
        Message::FaceCensorDone { count } => Text::key("faceCensorDone").count("count", *count as i64),
        Message::AnimationPreset(id) => Text::key(format!("animationPreset.{id}")).or_raw(*id),
        Message::PackageImportRefused(problem) => Text::key("packageImportRefused").arg("problem", import_problem(problem)),
        }
    }
}

fn stream_name(stream: StreamKind) -> Text {
    Text::key(match stream {
        StreamKind::Video => "stream.video",
        StreamKind::Audio => "stream.audio",
    })
}

/// The stream with its article where the language has one, for "has a video stream" and "has an audio stream".
fn stream_with_article(stream: StreamKind) -> Text {
    Text::key(match stream {
        StreamKind::Video => "streamWithArticle.video",
        StreamKind::Audio => "streamWithArticle.audio",
    })
}

/// A kind of track with its article where the language has one, for "a video track" and "an audio track".
fn track_kind_with_article(kind: engine::TrackKind) -> Text {
    Text::key(match kind {
        engine::TrackKind::Video => "trackKindWithArticle.video",
        engine::TrackKind::Audio => "trackKindWithArticle.audio",
        engine::TrackKind::Sticker => "trackKindWithArticle.sticker",
        engine::TrackKind::Text => "trackKindWithArticle.text",
    })
}

fn track_label(track: TrackLabel) -> Text {
    let (key, number) = match track {
        TrackLabel::Video(number) => ("trackLabel.video", number),
        TrackLabel::Audio(number) => ("trackLabel.audio", number),
        TrackLabel::Sticker(number) => ("trackLabel.sticker", number),
        TrackLabel::Text(number) => ("trackLabel.text", number),
        TrackLabel::Position(number) => ("trackLabel.position", number),
    };
    Text::key(key).arg("number", number)
}

/// A refused edit on `track`, whose label is the `track` argument.
fn on_track(key: &'static str, track: TrackLabel) -> Text {
    Text::key(key).arg("track", track_label(track))
}

fn clip_edge(edge: ClipEdge) -> Text {
    Text::key(match edge {
        ClipEdge::Start => "clipEdge.start",
        ClipEdge::End => "clipEdge.end",
    })
}

fn install_verb(replaced: bool) -> Text {
    Text::key(if replaced { "installVerb.replaced" } else { "installVerb.installed" })
}

fn cue_count(cues: usize) -> Text {
    Text::key("cueCount").count("count", cues as i64)
}

fn select_clip(action: ClipAction) -> Text {
    Text::key(match action {
        ClipAction::Split => "selectClip.split",
        ClipAction::Delete => "selectClip.delete",
        ClipAction::Change => "selectClip.change",
        ClipAction::Mute => "selectClip.mute",
        ClipAction::Trim => "selectClip.trim",
        ClipAction::AddEffect => "selectClip.addEffect",
        ClipAction::RemoveEffect => "selectClip.removeEffect",
        ClipAction::AddTransition => "selectClip.addTransition",
    })
}

fn pack_tab(tab: PackTab) -> Text {
    Text::key(match tab {
        PackTab::Effect => "packTab.effect",
        PackTab::Transition => "packTab.transition",
        PackTab::TextStyle => "packTab.textStyle",
        PackTab::SubtitleStyle => "packTab.subtitleStyle",
        PackTab::ClipAnimation => "packTab.clipAnimation",
        PackTab::Sticker => "packTab.sticker",
        PackTab::Sfx => "packTab.sfx",
        PackTab::Music => "packTab.music",
        PackTab::Font => "packTab.font",
    })
}

/// How long ago an autosave was written, in the largest whole unit.
fn recovery_ago(seconds: u64) -> Text {
    match seconds {
        0..60 => Text::key("recoveryAgo.lessThanMinute"),
        60..3600 => Text::key("recoveryAgo.minutes").count("count", (seconds / 60) as i64),
        3600..86400 => Text::key("recoveryAgo.hours").count("count", (seconds / 3600) as i64),
        _ => Text::key("recoveryAgo.days").count("count", (seconds / 86400) as i64),
    }
}

fn entry_problem(problem: EntryProblem) -> Text {
    Text::key(match problem {
        EntryProblem::UnsafePath => "entryProblem.unsafePath",
        EntryProblem::Symlink => "entryProblem.symlink",
        EntryProblem::NotAFile => "entryProblem.notAFile",
        EntryProblem::Colon => "entryProblem.colon",
        EntryProblem::InvalidCharacter => "entryProblem.invalidCharacter",
        EntryProblem::DeviceName => "entryProblem.deviceName",
        EntryProblem::TrailingDotOrSpace => "entryProblem.trailingDotOrSpace",
        EntryProblem::Duplicate => "entryProblem.duplicate",
        EntryProblem::Encrypted => "entryProblem.encrypted",
        EntryProblem::ExtensionNotAllowed => "entryProblem.extensionNotAllowed",
    })
}

fn package_kind_with_article(kind: PackageKind) -> Text {
    Text::key(match kind {
        PackageKind::Effect => "packageKindWithArticle.effect",
        PackageKind::Transition => "packageKindWithArticle.transition",
    })
}

fn import_problem(problem: &ImportProblem) -> Text {
    match problem {
        ImportProblem::Unreadable(detail) => Text::key("importProblem.unreadable").arg("detail", detail),
        ImportProblem::NotAZip(detail) => Text::key("importProblem.notAZip").arg("detail", detail),
        ImportProblem::Entry { entry, problem } => Text::key("importProblem.entry").arg("entry", entry).arg("problem", entry_problem(*problem)),
        ImportProblem::TooManyFiles { limit } => Text::key("importProblem.tooManyFiles").arg("limit", limit),
        ImportProblem::TooLarge { entry: Some(entry), limit_mib } => Text::key("importProblem.tooLargeEntry").arg("entry", entry).arg("limit_mib", limit_mib),
        ImportProblem::TooLarge { entry: None, limit_mib } => Text::key("importProblem.tooLarge").arg("limit_mib", limit_mib),
        ImportProblem::NoManifest => Text::key("importProblem.noManifest"),
        ImportProblem::BadManifest(detail) => Text::key("importProblem.badManifest").arg("detail", detail),
        ImportProblem::UnknownType(kind) => Text::key("importProblem.unknownType").arg("kind", kind),
        ImportProblem::NotSupportedYet(kind) => Text::key("importProblem.notSupportedYet").arg("kind", kind),
        ImportProblem::BadPackage(detail) => Text::key("importProblem.badPackage").arg("detail", detail),
        ImportProblem::NotUsable { inputs } => Text::key("importProblem.notUsable").arg("inputs", inputs),
        ImportProblem::KindChanged { existing, new } => Text::key("importProblem.kindChanged").arg("existing", package_kind_with_article(*existing)).arg("new", package_kind_with_article(*new)),
        ImportProblem::BadId(id) => Text::key("importProblem.badId").arg("id", id),
        ImportProblem::SizeMismatch { entry } => Text::key("importProblem.sizeMismatch").arg("entry", entry),
        ImportProblem::RatioTooHigh { entry, limit } => Text::key("importProblem.ratioTooHigh").arg("entry", entry).arg("limit", limit),
        ImportProblem::NoUserFolder => Text::key("importProblem.noUserFolder"),
        ImportProblem::WriteFailed(detail) => Text::key("importProblem.writeFailed").arg("detail", detail),
    }
}

#[cfg(test)]
mod tests {
    use super::catalog::Strings;
    use super::text::{placeholders, Arg};
    use super::*;
    use std::collections::BTreeSet;

    fn s() -> String {
        "a".to_owned()
    }

    fn vietnamese() -> Language {
        Language::from_code("vi").expect("vi.json is in the i18n folder of the checkout")
    }

    fn shipped(code: &str) -> Strings {
        let text = std::fs::read_to_string(Path::new(DEV_LANGUAGE_FOLDER).join(format!("{code}.json"))).unwrap();
        let (strings, skipped) = Strings::parse(&text).unwrap();
        assert!(skipped.is_empty(), "{code}.json: {skipped:?}");
        strings
    }

    /// One message of every variant, then the other values of the enums inside them and the
    /// counts and cases that pick another key.
    fn samples() -> Vec<Message> {
        let mut all = vec![
        Message::Technical(s()),
        Message::Ready,
        Message::Loading(s()),
        Message::RefusedLine(s()),
        Message::PreviewLine(s()),
        Message::AudioLine(s()),
        Message::NoSoundLine(s()),
        Message::AudioGapLine(2),
        Message::FinishDragFirst,
        Message::ExportRunning,
        Message::ExportAlreadyRunning,
        Message::NoNeighbour,
        Message::NothingAtPlayhead,
        Message::NothingToPlay,
        Message::NothingToUndo,
        Message::NothingToRedo,
        Message::Saved(s()),
        Message::Opened(s()),
        Message::Exporting,
        Message::Added(s()),
        Message::AddedWithLength { name: s(), seconds: s() },
        Message::AddedNoLength { name: s(), seconds: 2 },
        Message::StreamUnusable { name: s(), stream: StreamKind::Audio, reason: s() },
        Message::StreamMissing { name: s(), stream: StreamKind::Audio },
        Message::Imported(s()),
        Message::NoUsableStream(s()),
        Message::SelectClip(ClipAction::Split),
        Message::ExpressionDropped,
        Message::EditTrackNotFound(2),
        Message::EditClipNotFound { track: TrackLabel::Video(2), clip: 2 },
        Message::EditEffectNotFound { track: TrackLabel::Video(2), clip: 2, effect: 2 },
        Message::EditTransitionNotFound { track: TrackLabel::Video(2), after_clip: 2 },
        Message::EditTrackKindMismatch { from: engine::TrackKind::Audio, to: engine::TrackKind::Audio },
        Message::EditNoRoom(TrackLabel::Video(2)),
        Message::EditOverlap { track: TrackLabel::Video(2), start: s(), end: s() },
        Message::EditTimeOverflow { track: TrackLabel::Video(2), clip: 2 },
        Message::EditTransitionOnEdge { track: TrackLabel::Video(2), edge: ClipEdge::Start },
        Message::EditBeforeSourceStart(TrackLabel::Video(2)),
        Message::EditTrackLocked(TrackLabel::Video(2)),
        Message::EditSpeedOverlap { track: TrackLabel::Video(2), end: s(), next: s() },
        Message::EditSplitOutsideClip { track: TrackLabel::Video(2), at: s() },
        Message::EditSplitInsideFade { track: TrackLabel::Video(2), at: s() },
        Message::EditSplitInsideTransition { track: TrackLabel::Video(2), at: s() },
        Message::EditSplitInsideHold { track: TrackLabel::Video(2), at: s() },
        Message::EditInvalid(s()),
        Message::LanguageSaveFailed(s()),
        Message::PanelSizesSaveFailed(s()),
        Message::ResizePanelHint,
        Message::DialogOpen,
        Message::LoadRunning,
        Message::LoadDropped,
        Message::OpenCancelled,
        Message::OpenFailed(s()),
        Message::SaveFailed(s()),
        Message::TaskFailed(s()),
        Message::PromptDiscardForNew,
        Message::PromptDiscardForOpen,
        Message::PromptDiscardForClose,
        Message::PromptCancelExport,
        Message::NothingToExport,
        Message::ExportProgress { done: 2, total: 2 },
        Message::ExportFinished { frames: 2, path: s(), encoder: s() },
        Message::ExportFinishedSilent { frames: 2, path: s(), encoder: s() },
        Message::ExportCancelled,
        Message::ExportFailed(s()),
        Message::ExportToolMissing(s()),
        Message::ExportChildCrashed { code: s(), stderr: s() },
        Message::ExportRendererMismatch,
        Message::ExportResultUnknown(s()),
        Message::EncoderAuto,
        Message::EncoderNeedsGpl(s()),
        Message::EncoderMissing(s()),
        Message::EncoderProbing(s()),
        Message::EncoderUnavailable(s()),
        Message::FilterMedia,
        Message::FilterAudio,
        Message::FilterProject,
        Message::FilterAllFiles,
        Message::Dirty,
        Message::Untitled,
        Message::MenuFile,
        Message::MenuNewProject,
        Message::MenuOpen,
        Message::MenuSave,
        Message::MenuSaveAs,
        Message::MenuLanguage,
        Message::MenuTheme,
        Message::ThemeSystem,
        Message::ThemeDark,
        Message::ThemeLight,
        Message::ThemeSaveFailed(s()),
        Message::MenuAgentAccess,
        Message::AgentConnected,
        Message::ProjectInUse(s()),
        Message::AgentAccessFailed(s()),
        Message::MenuAutosave,
        Message::AutosaveOff,
        Message::AutosaveEvery(2),
        Message::AutosaveSettingFailed(s()),
        Message::AutosaveFailed(s()),
        Message::MenuPackageManager,
        Message::MenuSaveWithPacks,
        Message::PackManagerTitle,
        Message::PackTabName(crate::package_manager::PackTab::Effect),
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
        Message::PackRemoveQuestion(s()),
        Message::PackUsedBy(2),
        Message::PackUseClip { track: 2, clip: 2 },
        Message::PackUseTransition { track: 2, after_clip: 2 },
        Message::PackRemoved(s()),
        Message::PackRemoveFailed(s()),
        Message::PackToggleFailed(s()),
        Message::PackOpenFolderFailed(s()),
        Message::PackSaveWithPacksFailed(s()),
        Message::PackBackupFailed(s()),
        Message::PackRestoreOffer(2),
        Message::PackRestoreInstall,
        Message::PackRestoreDismiss,
        Message::PackRestored(2),
        Message::PackRestoreFailed(s()),
        Message::NoUserPackageFolder,
        Message::MenuProxy,
        Message::ProxyOff,
        Message::ProxyAuto,
        Message::ProxyOn,
        Message::ProxyCancel,
        Message::ProxyRetry,
        Message::ProxyMaking { left: 2, percent: 2 },
        Message::ProxyBadge,
        Message::ProxyQueued,
        Message::ProxyRunning(2),
        Message::ProxyReady,
        Message::ProxyFailed(s()),
        Message::ProxyCancelled,
        Message::ProxySettingFailed(s()),
        Message::RecoveryTitle,
        Message::RecoveryIntro,
        Message::RecoveryRestore,
        Message::RecoveryDiscard,
        Message::RecoveryLater,
        Message::RecoveryAgo(2),
        Message::RecoveryOriginalNewer,
        Message::RecoveryRestored(s()),
        Message::RecoveryFailed(s()),
        Message::RecoveryBusy,
        Message::RecoveryTaken(s()),
        Message::RecoveryKeptDays(2),
        Message::Cancel,
        Message::ExportVideo,
        Message::TabMedia,
        Message::TabAudio,
        Message::TabStickers,
        Message::SearchStickers,
        Message::AllCategories,
        Message::StickerLibraryEmpty,
        Message::StickerAddHint,
        Message::AnimatedSticker { loop_ms: 2, loops: 2 },
        Message::StickerPackInstalled { name: s(), replaced: true },
        Message::FontPackInstalled { name: s(), replaced: true },
        Message::ClipAnimationPackInstalled { name: s(), replaced: true },
        Message::TextStylePackInstalled { name: s(), replaced: true },
        Message::ReadingStickerPacks,
        Message::StickerPackProblems(2),
        Message::EditSpeedOfImage(TrackLabel::Video(2)),
        Message::EditKeyframeOutsideClip(TrackLabel::Video(2)),
        Message::EditKeyframeNotFound(TrackLabel::Video(2)),
        Message::EditNoPlacement(TrackLabel::Video(2)),
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
        Message::EditSpeedOfText(TrackLabel::Video(2)),
        Message::EditReverseOfStill(TrackLabel::Video(2)),
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
        Message::FontMissing(s()),
        Message::TextureMissing(s()),
        Message::FontTargetChanged,
        Message::ImportSubtitles,
        Message::FilterSubtitles,
        Message::SubtitlesImported { cues: 2, skipped: 2 },
        Message::SubtitlesNone,
        Message::SubtitleFileUnreadable(s()),
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
        Message::SubtitlesExported(s()),
        Message::SubtitleExportFailed(s()),
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
        Message::LayerLimit(2),
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
        Message::ClipTabProperties,
        Message::ClipTabPicture,
        Message::ClipTabGeneral,
        Message::TabAnimation,
        Message::HoldRemovedTransition,
        Message::TransitionReleasedHold,
        Message::AnimationHoldNote(s()),
        Message::AnimationDurationHint,
        Message::AnimationEnter,
        Message::AnimationExit,
        Message::AnimationRepeat,
        Message::AnimationNone,
        Message::FieldAnimationDuration,
        Message::AnimationPreset("fade"),
        Message::AnimationPackMissing(s()),
        Message::TabEffects,
        Message::Transitions,
        Message::ImportFromComputer,
        Message::ImportPackage,
        Message::FilterPackage,
        Message::PromptOverwritePackage(s()),
        Message::PackageInstalled { name: s(), transition: true, replaced: true },
        Message::PackageImportRefused(ImportProblem::NotAZip(s())),
        Message::AudioPackInstalled { name: s(), replaced: true },
        Message::SessionAudio,
        Message::InstalledPacks,
        Message::ReadingAudioPacks,
        Message::AudioLibraryEmpty,
        Message::AudioPackProblems(2),
        Message::PreviewSound,
        Message::StopPreview,
        Message::AddSound,
        Message::AudioAddHint,
        Message::SoundPreviewFailed(s()),
        Message::SearchMedia,
        Message::SearchAudio,
        Message::SearchEffects,
        Message::LibraryEmpty,
        Message::NoSearchMatch,
        Message::AddHint,
        Message::ReadingPackages,
        Message::NoPackages(s()),
        Message::AddToClip,
        Message::PackageProblems(2),
        Message::PreviewFailed(s()),
        Message::PreviewUnavailable(s()),
        Message::ClipTitleVideo,
        Message::ClipTitleAudio,
        Message::ClipTitleImage,
        Message::ClipTitleSticker,
        Message::ClipTitleText,
        Message::TransitionTitle,
        Message::ProjectTitle,
        Message::FieldSize,
        Message::FieldFrameRate,
        Message::FieldAspect,
        Message::FieldDuration,
        Message::FieldSpeed,
        Message::FieldStart,
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
        Message::DropOnTimeline,
        Message::OutputSizeCustom,
        Message::FrameRateCustom,
        Message::Apply,
        Message::FieldVolume,
        Message::SelectClipHint,
        Message::Mute,
        Message::Reverse,
        Message::FadeIn,
        Message::FadeOut,
        Message::FieldDenoise,
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
        Message::MatteTemporal,
        Message::MatteTemporalHint,
        Message::MatteInvert,
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
        Message::MatteImported,
        Message::MatteImportFailed(s()),
        Message::FilterMatteModel,
        Message::DenoiseOff,
        Message::DenoiseVoice,
        Message::DenoiseGeneral,
        Message::DenoiseVoiceHint,
        Message::DenoiseGeneralHint,
        Message::DenoiseStrength,
        Message::FieldVoiceChange,
        Message::VoiceNormal,
        Message::VoiceChipmunk,
        Message::VoiceDeep,
        Message::VoiceRobot,
        Message::FieldPitch,
        Message::FieldBeats,
        Message::DetectBeats,
        Message::DetectBeatsHint,
        Message::ClearBeats,
        Message::CancelBeats,
        Message::BeatsDetecting(2),
        Message::BeatsSummary { count: 2, tempo: s() },
        Message::BeatsNone,
        Message::BeatsFound { count: 2, tempo: s() },
        Message::BeatsNotFound,
        Message::BeatsKeptForRedo,
        Message::BeatsFailed(s()),
        Message::TrackingDone { count: 2 },
        Message::TrackingLost { count: 2 },
        Message::TrackingCancelled { count: 2 },
        Message::TrackingFailed(s()),
        Message::TrackingStale,
        Message::TrackCreated,
        Message::FollowNoReference,
        Message::FollowBaked,
        Message::FieldTracking,
        Message::TrackPick,
        Message::TrackPickHint,
        Message::TrackPicking,
        Message::TrackForward,
        Message::TrackBackward,
        Message::TrackOneFrame,
        Message::TrackStop,
        Message::TrackDelete,
        Message::TrackNone,
        Message::TrackName { id: 2, count: 2 },
        Message::TrackRunning(2),
        Message::TrackSmooth,
        Message::TrackHintFix,
        Message::TrackMode,
        Message::TrackModeFast,
        Message::TrackModeFastHint,
        Message::TrackModeRobust,
        Message::TrackModeRobustHint,
        Message::TrackModeRobustPlanar,
        Message::TrackModelMissing,
        Message::TrackModelInstalled,
        Message::FaceModelInstalled,
        Message::FaceModelMissing,
        Message::FaceCensor,
        Message::FaceCensorHint,
        Message::FaceCensorRunning(2),
        Message::FaceCensorDone { count: 2 },
        Message::FaceCensorNone,
        Message::FaceCensorCancelled,
        Message::FaceCensorStale,
        Message::FaceCensorFailed(s()),
        Message::FilterTrackingModel,
        Message::AddCensorFollowing,
        Message::FollowTrack,
        Message::FollowScale,
        Message::FollowRotation,
        Message::FollowPerspective,
        Message::FollowPerspectiveHint,
        Message::FollowBakePerspective,
        Message::TrackPlanar,
        Message::TrackPlanarHint,
        Message::FollowClear,
        Message::FollowBake,
        Message::FollowingTrack(2),
        Message::LockToObject,
        Message::LockedToTrack(2),
        Message::LockScale,
        Message::LockRotation,
        Message::LockPerspectiveHint,
        Message::LockPerspectiveNeedsPlanar,
        Message::LockZoom,
        Message::LockClear,
        Message::LockBaked,
        Message::EffectRegion,
        Message::RegionCenterX,
        Message::RegionCenterY,
        Message::RegionWidth,
        Message::RegionHeight,
        Message::RegionLinkNone,
        Message::MarkerAddShortcut,
        Message::MarkerHint,
        Message::MarkerRemove,
        Message::MarkerLabel,
        Message::MarkerColor,
        Message::MarkerDefaultColor,
        Message::NoEffectsYet,
        Message::TrackLockedNote,
        Message::SpeedBlockedByEndTransition,
        Message::Remove,
        Message::Reset,
        Message::PackageParamsUnreadable,
        Message::Variables(s()),
        Message::Player,
        Message::Fullscreen,
        Message::ExitFullscreen,
        Message::PlayShortcut,
        Message::StopShortcut,
        Message::UndoShortcut,
        Message::RedoShortcut,
        Message::SplitShortcut,
        Message::TrimBefore,
        Message::TrimAfter,
        Message::SnapToggle,
        Message::DeleteShortcut,
        Message::ZoomIn,
        Message::ZoomOut,
        Message::AddTrack,
        Message::AddVideoTrack,
        Message::AddAudioTrack,
        Message::AddStickerTrack,
        Message::AddTextTrack,
        Message::VideoTrackNumber(2),
        Message::AudioTrackNumber(2),
        Message::StickerTrackNumber(2),
        Message::TextTrackNumber(2),
        Message::AddClipFromFile,
        Message::LockTrack,
        Message::UnlockTrack,
        Message::HideTrack,
        Message::DeleteTrack,
        Message::ShowTrack,
        ];
        all.push(Message::StreamMissing { name: s(), stream: StreamKind::Video });
        all.push(Message::StreamUnusable { name: s(), stream: StreamKind::Video, reason: s() });
        for track in [TrackLabel::Video(1), TrackLabel::Audio(2), TrackLabel::Sticker(3), TrackLabel::Text(4), TrackLabel::Position(5)] {
            all.push(Message::EditNoRoom(track));
        }
        all.push(Message::EditTransitionOnEdge { track: TrackLabel::Audio(1), edge: ClipEdge::End });
        for action in [ClipAction::Split, ClipAction::Delete, ClipAction::Change, ClipAction::Mute, ClipAction::Trim, ClipAction::AddEffect, ClipAction::RemoveEffect, ClipAction::AddTransition] {
            all.push(Message::SelectClip(action));
        }
        let kinds = [engine::TrackKind::Video, engine::TrackKind::Audio, engine::TrackKind::Sticker, engine::TrackKind::Text];
        for from in kinds {
            for to in kinds {
                all.push(Message::EditTrackKindMismatch { from, to });
            }
        }
        use crate::package_manager::PackTab as Tab;
        for tab in [Tab::Effect, Tab::Transition, Tab::TextStyle, Tab::SubtitleStyle, Tab::ClipAnimation, Tab::Sticker, Tab::Sfx, Tab::Music, Tab::Font] {
            all.push(Message::PackTabName(tab));
        }
        let entry_problems = [
            EntryProblem::UnsafePath,
            EntryProblem::Symlink,
            EntryProblem::NotAFile,
            EntryProblem::Colon,
            EntryProblem::InvalidCharacter,
            EntryProblem::DeviceName,
            EntryProblem::TrailingDotOrSpace,
            EntryProblem::Duplicate,
            EntryProblem::Encrypted,
            EntryProblem::ExtensionNotAllowed,
        ];
        let mut problems = vec![
            ImportProblem::Unreadable(s()),
            ImportProblem::NotAZip(s()),
            ImportProblem::TooManyFiles { limit: 4 },
            ImportProblem::TooLarge { entry: Some(s()), limit_mib: 5 },
            ImportProblem::TooLarge { entry: None, limit_mib: 5 },
            ImportProblem::NoManifest,
            ImportProblem::BadManifest(s()),
            ImportProblem::UnknownType(s()),
            ImportProblem::NotSupportedYet(s()),
            ImportProblem::BadPackage(s()),
            ImportProblem::NotUsable { inputs: 3 },
            ImportProblem::KindChanged { existing: PackageKind::Effect, new: PackageKind::Transition },
            ImportProblem::KindChanged { existing: PackageKind::Transition, new: PackageKind::Effect },
            ImportProblem::BadId(s()),
            ImportProblem::NoUserFolder,
            ImportProblem::WriteFailed(s()),
            ImportProblem::SizeMismatch { entry: s() },
            ImportProblem::RatioTooHigh { entry: s(), limit: 200 },
        ];
        problems.extend(entry_problems.into_iter().map(|problem| ImportProblem::Entry { entry: s(), problem }));
        all.extend(problems.into_iter().map(Message::PackageImportRefused));
        for count in [0usize, 1, 2] {
            let wide = count as i64;
            all.extend([
                Message::ExportFinished { frames: wide, path: s(), encoder: s() },
                Message::ExportFinishedSilent { frames: wide, path: s(), encoder: s() },
                Message::PackUsedBy(count),
                Message::PackRestoreOffer(count),
                Message::PackRestored(count),
                Message::StickerPackProblems(count),
                Message::TextStylePackProblems(count),
                Message::AudioPackProblems(count),
                Message::PackageProblems(count),
                Message::SubtitlesImported { cues: count, skipped: 0 },
                Message::SubtitlesImported { cues: 2, skipped: count },
                Message::BeatsSummary { count, tempo: s() },
                Message::BeatsFound { count, tempo: s() },
                Message::TrackingDone { count },
                Message::TrackingLost { count },
                Message::TrackingCancelled { count },
                Message::TrackName { id: 1, count },
                Message::FaceCensorDone { count },
                Message::AnimatedSticker { loop_ms: 1250, loops: count as u32 },
            ]);
        }
        all.extend([
            Message::ExportChildCrashed { code: s(), stderr: String::new() },
            Message::RecoveryRestored(String::new()),
            Message::StickerPackInstalled { name: s(), replaced: false },
            Message::FontPackInstalled { name: s(), replaced: false },
            Message::TextStylePackInstalled { name: s(), replaced: false },
            Message::ClipAnimationPackInstalled { name: s(), replaced: false },
            Message::AudioPackInstalled { name: s(), replaced: false },
            Message::PackageInstalled { name: s(), transition: false, replaced: false },
            Message::PackageInstalled { name: s(), transition: true, replaced: true },
            Message::AnimationPreset("unknown-id"),
        ]);
        all.extend([1, 30, 59, 60, 61, 120, 300, 3600].map(Message::AutosaveEvery));
        all.extend([0, 59, 60, 119, 120, 3599, 3600, 7199, 7200, 86399, 86400, 172_799, 172_800, 900_000].map(Message::RecoveryAgo));
        all.extend(engine::ANIMATION_PRESETS.iter().map(|preset| Message::AnimationPreset(preset.id)));
        all
    }

    /// The keys `text` asks for, with the names of its arguments and whether it has a count.
    fn walk(text: &Text, found: &mut Vec<(String, Vec<&'static str>, bool)>) {
        if let Text::Key { key, args, .. } = text {
            found.push((key.to_string(), args.iter().map(|(name, _)| *name).collect(), text.plural_count().is_some()));
            for (_, arg) in args {
                if let Arg::Text(inner) = arg {
                    walk(inner, found);
                }
            }
        }
    }

    /// Every key the samples ask for, but the preset id no language knows.
    fn keys_asked_for() -> Vec<(String, Vec<&'static str>, bool)> {
        let mut found = Vec::new();
        for message in samples() {
            walk(&message.to_text(), &mut found);
        }
        found.retain(|(key, _, _)| key != "animationPreset.unknown-id");
        found
    }

    /// The templates of `key` in `strings`: the plural forms when it has a count, else the key.
    fn forms<'a>(strings: &'a Strings, key: &str, plural: bool) -> Vec<(String, &'a str)> {
        let names: Vec<String> = if plural { vec![format!("{key}.one"), format!("{key}.other")] } else { vec![key.to_owned()] };
        names.into_iter().filter_map(|name| strings.get(&name).map(|text| (name, text))).collect()
    }

    #[test]
    fn every_key_the_code_asks_for_is_in_english_with_the_arguments_it_gets() {
        let english = shipped("en");
        assert_eq!(&english, catalog().english(), "the built-in English is the shipped en.json");
        for (key, args, plural) in keys_asked_for() {
            let templates = forms(&english, &key, plural);
            assert!(!templates.is_empty(), "en.json has no {key}{}", if plural { ".one/.other" } else { "" });
            if plural {
                assert!(english.get(&format!("{key}.other")).is_some(), "en.json has no {key}.other");
            }
            for (name, template) in templates {
                for placeholder in placeholders(template) {
                    assert!(args.contains(&placeholder.as_str()), "en.json {name} uses {{{placeholder}}}, which the code does not pass (it passes {args:?})");
                }
            }
        }
    }

    /// Catches a new message whose key was not added to en.json even when no sample names it.
    #[test]
    fn every_literal_key_of_the_mapping_is_in_english() {
        let source = include_str!("mod.rs");
        let mapping = &source[..source.find("mod tests {").unwrap()];
        let english = shipped("en");
        let mut count = 0;
        for piece in mapping.split("Text::key(\"").skip(1) {
            let key = &piece[..piece.find('"').unwrap()];
            count += 1;
            assert!(english.get(key).is_some() || english.get(&format!("{key}.other")).is_some(), "en.json has no {key}");
        }
        assert!(count > 500, "the mapping was not found: {count} keys");
    }

    #[test]
    fn every_english_key_is_used() {
        let english = shipped("en");
        let used: BTreeSet<String> = keys_asked_for().into_iter().flat_map(|(key, _, plural)| if plural { vec![format!("{key}.one"), format!("{key}.other")] } else { vec![key] }).collect();
        let unused: Vec<&str> = english.keys().filter(|key| !used.contains(*key)).collect();
        assert!(unused.is_empty(), "keys of en.json that no message asks for: {unused:?}");
    }

    #[test]
    fn every_vietnamese_key_is_in_english_and_uses_only_its_placeholders() {
        let english = shipped("en");
        let vietnamese = shipped("vi");
        assert_eq!(vietnamese.name(), Some("Tiếng Việt"));
        for key in vietnamese.keys() {
            let base = key.strip_suffix(".one").or_else(|| key.strip_suffix(".other"));
            let english_templates: Vec<&str> = match base {
                Some(base) if english.get(key).is_none() => forms(&english, base, true).into_iter().map(|(_, text)| text).collect(),
                Some(base) => forms(&english, base, true).into_iter().map(|(_, text)| text).chain(english.get(key)).collect(),
                None => english.get(key).into_iter().collect(),
            };
            assert!(!english_templates.is_empty(), "vi.json has {key}, which en.json does not");
            let allowed: BTreeSet<String> = english_templates.iter().flat_map(|template| placeholders(template)).collect();
            for placeholder in placeholders(vietnamese.get(key).unwrap()) {
                assert!(allowed.contains(&placeholder), "vi.json {key} uses {{{placeholder}}}, which en.json does not");
            }
        }
    }

    #[test]
    fn the_shipped_files_load_without_problems() {
        assert!(catalog().problems().is_empty(), "{:?}", catalog().problems());
        assert_eq!(Language::ENGLISH.code(), "en");
        assert_eq!(Language::ENGLISH.native_name(), "English");
        assert_eq!(vietnamese().native_name(), "Tiếng Việt");
    }

    #[test]
    fn every_language_has_a_unique_code_that_reads_back() {
        for language in Language::all() {
            assert_eq!(Language::from_code(language.code()), Some(language));
        }
        assert_eq!(Language::from_code("xx"), None);
        assert_eq!(Language::all().next(), Some(Language::ENGLISH));
        assert_eq!(Language::default(), Language::ENGLISH);
    }

    /// A message without a key does not compile, as the mapping is an exhaustive match. This
    /// catches the other mistake: a text that was filled in with nothing.
    #[test]
    fn no_text_is_empty_in_any_language() {
        for language in Language::all() {
            for message in samples() {
                assert!(!language.text(&message).trim().is_empty(), "{language:?}: {message:?}");
            }
        }
    }

    #[test]
    fn english_counts_and_articles_read_right() {
        let finished = |frames| Language::ENGLISH.t(Message::ExportFinished { frames, path: "a.mp4".to_owned(), encoder: "x".to_owned() });
        assert!(finished(1).contains("Exported 1 frame to"), "{}", finished(1));
        assert!(finished(2).contains("Exported 2 frames to"), "{}", finished(2));
        assert!(finished(0).contains("Exported 0 frames to"), "{}", finished(0));
        assert!(Language::ENGLISH.t(Message::ExportFinishedSilent { frames: 1, path: "a.mp4".to_owned(), encoder: "x".to_owned() }).contains("Exported 1 frame to"));
        assert_eq!(Language::ENGLISH.t(Message::StickerPackProblems(1)), "1 pack or sticker could not be read");
        assert_eq!(vietnamese().t(Message::StickerPackProblems(2)), "Không đọc được 2 gói hoặc sticker");
        assert_eq!(vietnamese().t(Message::ReadingStickerPacks), "Đang đọc các gói sticker đã cài...");
        assert_eq!(Language::ENGLISH.t(Message::PackageProblems(1)), "1 package could not be read");
        assert_eq!(Language::ENGLISH.t(Message::PackageProblems(3)), "3 packages could not be read");
        let unusable = |stream| Language::ENGLISH.t(Message::StreamUnusable { name: "a".to_owned(), stream, reason: "r".to_owned() });
        assert_eq!(unusable(StreamKind::Audio), "a has an audio stream it cannot use: r");
        assert_eq!(unusable(StreamKind::Video), "a has a video stream it cannot use: r");
        assert_eq!(vietnamese().t(Message::StreamUnusable { name: "a".to_owned(), stream: StreamKind::Audio, reason: "r".to_owned() }), "a có luồng âm thanh không dùng được: r");
        assert_eq!(Language::ENGLISH.t(Message::AutosaveEvery(60)), "Every minute");
        assert_eq!(vietnamese().t(Message::AutosaveEvery(60)), "Mỗi 1 phút");
    }

    #[test]
    fn vietnamese_split_is_told_apart_from_trim() {
        let vietnamese = vietnamese();
        assert!(vietnamese.t(Message::SplitShortcut).contains("Tách"));
        assert!(vietnamese.t(Message::SelectClip(ClipAction::Split)).contains("tách"));
        assert!(vietnamese.t(Message::TrimBefore).contains("Cắt"));
        assert_eq!(vietnamese.t(Message::AudioGapLine(5)), "Mất tiếng: 5 ms");
    }

    #[test]
    fn a_text_clip_is_not_called_an_image_and_presets_have_names() {
        let vietnamese_text = vietnamese().t(Message::EditSpeedOfText(TrackLabel::Video(1)));
        assert!(vietnamese_text.contains("chữ") && !vietnamese_text.contains("ảnh"), "{vietnamese_text}");
        let english = Language::ENGLISH.t(Message::EditSpeedOfText(TrackLabel::Video(1)));
        assert!(english.contains("text") && !english.contains("image"), "{english}");
        assert_eq!(Language::ENGLISH.t(Message::AnimationPreset("slide-left")), "Slide left");
        assert_eq!(vietnamese().t(Message::AnimationPreset("fade")), "Mờ dần");
        assert_eq!(Language::ENGLISH.t(Message::AnimationPreset("new-one")), "new-one", "an unknown id shows as it is");
        for preset in engine::ANIMATION_PRESETS {
            assert_ne!(vietnamese().t(Message::AnimationPreset(preset.id)), preset.id, "{} has a Vietnamese name", preset.id);
        }
    }

    #[test]
    fn technical_text_is_passed_through_as_it_is() {
        for language in Language::all() {
            assert_eq!(language.t(Message::Technical("avcodec failed {x}".to_owned())), "avcodec failed {x}");
        }
    }

    #[test]
    fn arguments_are_not_read_as_templates() {
        assert_eq!(Language::ENGLISH.t(Message::Added("{name}}{{".to_owned())), "Added {name}}{{");
    }

    #[test]
    fn the_two_languages_differ_and_parameters_arrive() {
        assert_ne!(Language::ENGLISH.t(Message::MenuFile), vietnamese().t(Message::MenuFile));
        for language in Language::all() {
            assert!(language.t(Message::Added("clip.mp4".to_owned())).contains("clip.mp4"));
            assert!(language.t(Message::ExportProgress { done: 3, total: 9 }).contains('9'));
        }
    }
}
