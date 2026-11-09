//! Every text the editor shows. A [`Message`] says what to tell the user; each language is one table
//! with an exhaustive `match`, so a language lacking a string does not compile. Adding a language
//! is one table and one line in [`Language`]. Technical detail from the engine or FFmpeg travels
//! as [`Message::Technical`] or as the argument of a translated prefix, in English.

mod en;
mod vi;

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
    /// Text that is not translated: an encoder name, or engine detail under a translated prefix.
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
    LanguageSaveFailed(String),

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
    Cancel,
    ExportVideo,

    // Left panel.
    TabMedia,
    TabAudio,
    TabEffects,
    Transitions,
    ImportFromComputer,
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

    // Inspector.
    ClipTitle,
    ProjectTitle,
    FieldSize,
    FieldFrameRate,
    FieldAspect,
    FieldDuration,
    FieldStart,
    FieldVolume,
    SelectClipHint,
    Mute,
    FadeIn,
    FadeOut,
    NoEffectsYet,
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
    DeleteShortcut,
    ZoomIn,
    ZoomOut,
    AddTrack,
    AddVideoTrack,
    AddAudioTrack,
    VideoTrackNumber(usize),
    AudioTrackNumber(usize),
    AddClipFromFile,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Message::MenuFile,
            Message::TabMedia,
            Message::FieldDuration,
            Message::AddTrack,
        ];
        for language in Language::ALL {
            for message in &all {
                assert!(!language.text(message).trim().is_empty(), "{language:?}: {message:?}");
            }
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
