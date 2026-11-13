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
    EditTrackKindMismatch { from: StreamKind, to: StreamKind },
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
    StickerPackInstalled { name: String, replaced: bool },
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
    FieldTextContent,
    FieldFont,
    DefaultFont,
    ChooseFontFile,
    FilterFont,
    /// The font list is not read yet.
    LoadingFonts,
    /// The font a text asks for is not installed and the default one draws it.
    FontMissing(String),
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
    TabAnimation,
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
    ClipTitle,
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
    FadeIn,
    FadeOut,
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
    VideoTrackNumber(usize),
    AudioTrackNumber(usize),
    AddClipFromFile,
    LockTrack,
    UnlockTrack,
    HideTrack,
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
            Message::StickerPackInstalled { name: name(), replaced: true },
            Message::ClipAnimationPackInstalled { name: name(), replaced: false },
            Message::TextStylePackInstalled { name: name(), replaced: true },
            Message::ReadingStickerPacks,
            Message::StickerPackProblems(2),
            Message::EditSpeedOfImage(TrackLabel::Video(1)),
            Message::EditKeyframeOutsideClip(TrackLabel::Video(1)),
            Message::EditKeyframeNotFound(TrackLabel::Video(1)),
            Message::EditNoPlacement(TrackLabel::Video(1)),
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
            Message::FieldTextContent,
            Message::FieldFont,
            Message::DefaultFont,
            Message::ChooseFontFile,
            Message::FilterFont,
            Message::LoadingFonts,
            Message::FontMissing(name()),
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
            Message::ShowTrack,
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
