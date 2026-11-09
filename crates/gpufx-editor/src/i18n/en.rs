//! English strings. No `_` arm: a new message must be given text here.

use super::{ClipAction, ClipEdge, Message, StreamKind, TrackLabel};

fn stream_name(stream: StreamKind) -> &'static str {
    match stream {
        StreamKind::Video => "video",
        StreamKind::Audio => "audio",
    }
}

/// The stream with its article, for "has a video stream" and "has an audio stream".
fn stream_with_article(stream: StreamKind) -> &'static str {
    match stream {
        StreamKind::Video => "a video",
        StreamKind::Audio => "an audio",
    }
}

/// `count` and `noun`, plural unless the count is one.
fn counted(count: i64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn track_label(track: TrackLabel) -> String {
    match track {
        TrackLabel::Video(number) => format!("Video track {number}"),
        TrackLabel::Audio(number) => format!("Audio track {number}"),
        TrackLabel::Position(number) => format!("Track {number}"),
    }
}

fn select_clip(action: ClipAction) -> &'static str {
    match action {
        ClipAction::Split => "Select a clip to split",
        ClipAction::Delete => "Select a clip to delete",
        ClipAction::Change => "Select a clip to change",
        ClipAction::Mute => "Select a clip to mute",
        ClipAction::Trim => "Select a clip to trim",
        ClipAction::AddEffect => "Select a clip to add an effect",
        ClipAction::RemoveEffect => "Select a clip to remove an effect",
        ClipAction::AddTransition => "Select a clip to add a transition",
    }
}

pub(super) fn text(message: &Message) -> String {
    match message {
        Message::Technical(text) => text.clone(),

        Message::Ready => "Ready".to_owned(),
        Message::Loading(name) => format!("Loading {name}\u{2026}"),
        Message::RefusedLine(detail) => format!("Refused: {detail}"),
        Message::PreviewLine(detail) => format!("Preview: {detail}"),
        Message::AudioLine(detail) => format!("Audio: {detail}"),
        Message::NoSoundLine(detail) => format!("No sound: {detail}"),
        Message::AudioGapLine(ms) => format!("Audio gaps: {ms} ms"),
        Message::FinishDragFirst => "Finish the drag first".to_owned(),
        Message::ExportRunning => "Editing is off while an export runs".to_owned(),
        Message::ExportAlreadyRunning => "An export is already running; wait for it to finish".to_owned(),
        Message::NoNeighbour => "A transition needs a selected clip that touches another clip on its track".to_owned(),
        Message::NothingAtPlayhead => "Move the playhead inside the selected clip first".to_owned(),
        Message::NothingToPlay => "Nothing to play: playback length follows the video frames, and audio alone has none yet".to_owned(),
        Message::NothingToUndo => "Nothing to undo".to_owned(),
        Message::NothingToRedo => "Nothing to redo".to_owned(),
        Message::Saved(path) => format!("Saved {path}"),
        Message::Opened(path) => format!("Opened {path}"),
        Message::Exporting => "Exporting".to_owned(),
        Message::Added(name) => format!("Added {name}"),
        Message::AddedWithLength { name, seconds } => format!("Added {name}, {seconds} s"),
        Message::AddedNoLength { name, seconds } => format!("Added {name}: it states no length, so it got {seconds} s"),
        Message::StreamUnusable { name, stream, reason } => format!("{name} has {} stream it cannot use: {reason}", stream_with_article(*stream)),
        Message::StreamMissing { name, stream } => format!("{name} has no {} stream", stream_name(*stream)),
        Message::Imported(name) => format!("Imported {name}"),
        Message::NoUsableStream(name) => format!("{name} has no usable video or audio"),
        Message::SelectClip(action) => select_clip(*action).to_owned(),
        Message::ExpressionDropped => "The expression being typed was dropped: the clip changed".to_owned(),
        Message::LanguageSaveFailed(detail) => format!("The language could not be saved: {detail}"),
        Message::PanelSizesSaveFailed(detail) => format!("The panel sizes could not be saved: {detail}"),
        Message::ResizePanelHint => "Drag to resize, double-click to reset".to_owned(),

        Message::EditTrackNotFound(number) => format!("Track {number} does not exist"),
        Message::EditClipNotFound { track, clip } => format!("{}: clip {clip} does not exist", track_label(*track)),
        Message::EditEffectNotFound { track, clip, effect } => format!("{}, clip {clip}: effect {effect} does not exist", track_label(*track)),
        Message::EditTransitionNotFound { track, after_clip } => format!("{}: no transition after clip {after_clip}", track_label(*track)),
        Message::EditTrackKindMismatch { from, to } => format!("A clip cannot move from {} track to {} track", stream_with_article(*from), stream_with_article(*to)),
        Message::EditNoRoom(track) => format!("{}: no room for the clip", track_label(*track)),
        Message::EditOverlap { track, start, end } => format!("{}: {start} s to {end} s overlaps another clip", track_label(*track)),
        Message::EditTimeOverflow { track, clip } => format!("{}, clip {clip}: the time is out of range", track_label(*track)),
        Message::EditTransitionOnEdge { track, edge } => format!(
            "{}: the {} of the clip holds a transition, so it cannot be trimmed. Remove the transition first",
            track_label(*track),
            match edge {
                ClipEdge::Start => "start",
                ClipEdge::End => "end",
            }
        ),
        Message::EditBeforeSourceStart(track) => format!("{}: the clip cannot start before the beginning of its source", track_label(*track)),
        Message::EditSplitOutsideClip { track, at } => format!("{}: cannot split at {at} s", track_label(*track)),
        Message::EditSplitInsideFade { track, at } => format!("{}: {at} s is inside a fade", track_label(*track)),
        Message::EditSplitInsideTransition { track, at } => format!("{}: {at} s is inside a transition", track_label(*track)),
        Message::EditTrackLocked(track) => format!("{}: the track is locked", track_label(*track)),
        Message::EditSpeedOverlap { track, end, next } => format!("{}: at this speed the clip would end at {end} s, past the next clip at {next} s", track_label(*track)),
        Message::EditInvalid(detail) => format!("The result is not valid: {detail}"),

        Message::DialogOpen => "Close the open dialog first".to_owned(),
        Message::LoadRunning => "Wait for the current load to finish".to_owned(),
        Message::LoadDropped => "The load in progress was dropped".to_owned(),
        Message::OpenCancelled => "The project changed while it was being read, so Open was cancelled".to_owned(),
        Message::OpenFailed(detail) => format!("Could not open the project: {detail}"),
        Message::SaveFailed(detail) => format!("Could not save the project: {detail}"),
        Message::TaskFailed(detail) => format!("A background task failed: {detail}"),

        Message::PromptDiscardForNew => "The project has unsaved changes. Create a new project and discard them?".to_owned(),
        Message::PromptDiscardForOpen => "The project has unsaved changes. Open another project and discard them?".to_owned(),
        Message::PromptDiscardForClose => "The project has unsaved changes. Close and discard them?".to_owned(),
        Message::PromptCancelExport => "An export is running. Cancel it and close?".to_owned(),

        Message::NothingToExport => "Nothing to export: the timeline has no video frames".to_owned(),
        Message::ExportProgress { done, total } => format!("Exporting {done} / {total}"),
        Message::ExportFinished { frames, path, encoder } => format!("Exported {} to {path} with {encoder}.", counted(*frames, "frame")),
        Message::ExportFinishedSilent { frames, path, encoder } => format!("Exported {} to {path} with {encoder}. The project has no audio output, so the file has no sound.", counted(*frames, "frame")),
        Message::ExportCancelled => "Export cancelled; the partial file was removed".to_owned(),
        Message::ExportFailed(detail) => format!("Export failed: {detail}"),
        Message::ExportToolMissing(path) => format!("Export failed: the renderer {path} is missing. Build gpufx-render or reinstall the app."),
        Message::ExportChildCrashed { code, stderr } if stderr.is_empty() => format!("Export failed: the renderer stopped unexpectedly (exit code {code})."),
        Message::ExportChildCrashed { code, stderr } => format!("Export failed: the renderer stopped unexpectedly (exit code {code}): {stderr}"),
        Message::ExportResultUnknown(files) => format!("The renderer's result could not be read. Check the output file; these temporary files were kept: {files}"),
        Message::ExportRendererMismatch => "Export failed: gpufx-render does not match this version of the editor. Install the matching gpufx-render.".to_owned(),
        Message::EncoderAuto => "Auto".to_owned(),
        Message::EncoderNeedsGpl(name) => format!("{name} (needs the GPL build)"),
        Message::EncoderMissing(name) => format!("{name} (not in this build)"),

        Message::FilterMedia => "Media".to_owned(),
        Message::FilterAudio => "Audio".to_owned(),
        Message::FilterProject => "Project".to_owned(),
        Message::FilterAllFiles => "All files".to_owned(),

        Message::Dirty => "Edited".to_owned(),
        Message::Untitled => "Untitled".to_owned(),
        Message::MenuFile => "File".to_owned(),
        Message::MenuNewProject => "New project".to_owned(),
        Message::MenuOpen => "Open...".to_owned(),
        Message::MenuSave => "Save".to_owned(),
        Message::MenuSaveAs => "Save as...".to_owned(),
        Message::MenuAgentAccess => "Let agents control this window".to_owned(),
        Message::AgentConnected => "Agent connected".to_owned(),
        Message::ProjectInUse(path) => format!("{path} is open in another process"),
        Message::AgentAccessFailed(detail) => format!("Could not open the agent connection: {detail}"),
        Message::MenuLanguage => "Language".to_owned(),
        Message::MenuTheme => "Theme".to_owned(),
        Message::ThemeSystem => "System".to_owned(),
        Message::ThemeDark => "Dark".to_owned(),
        Message::ThemeLight => "Light".to_owned(),
        Message::ThemeSaveFailed(detail) => format!("The theme could not be saved: {detail}"),
        Message::Cancel => "Cancel".to_owned(),
        Message::ExportVideo => "Export video".to_owned(),

        Message::TabMedia => "Media".to_owned(),
        Message::TabAudio => "Audio".to_owned(),
        Message::TabEffects => "Effects".to_owned(),
        Message::Transitions => "Transitions".to_owned(),
        Message::ImportFromComputer => "Import from computer".to_owned(),
        Message::SearchMedia => "Search media".to_owned(),
        Message::SearchAudio => "Search audio".to_owned(),
        Message::SearchEffects => "Search effects".to_owned(),
        Message::LibraryEmpty => "No files yet. Import a file to add it here.".to_owned(),
        Message::NoSearchMatch => "No file matches the search.".to_owned(),
        Message::AddHint => "Click a tile or its + to add it at the playhead.".to_owned(),
        Message::ReadingPackages => "Reading the package list...".to_owned(),
        Message::NoPackages(variable) => format!("No packages found. Set {variable}."),
        Message::AddToClip => "Add to selected clip".to_owned(),
        Message::PackageProblems(count) => format!("{} could not be read", counted(*count as i64, "package")),

        Message::ClipTitle => "Clip".to_owned(),
        Message::ProjectTitle => "Project".to_owned(),
        Message::FieldSize => "Size".to_owned(),
        Message::FieldFrameRate => "Frame rate".to_owned(),
        Message::FieldAspect => "Aspect".to_owned(),
        Message::FieldDuration => "Duration".to_owned(),
        Message::FieldSpeed => "Speed".to_owned(),
        Message::FieldStart => "Timeline start".to_owned(),
        Message::DropOnTimeline => "Drop files on the timeline to add them".to_owned(),
        Message::OutputSizeCustom => "Custom size".to_owned(),
        Message::Apply => "Apply".to_owned(),
        Message::FieldSourceIn => "Start of video".to_owned(),
        Message::FieldVideoDuration => "Duration of video".to_owned(),
        Message::FieldPlacement => "Position and size".to_owned(),
        Message::FieldVolume => "Volume".to_owned(),
        Message::SelectClipHint => "Select a clip on the timeline to edit it.".to_owned(),
        Message::Mute => "Mute".to_owned(),
        Message::FadeIn => "Fade in".to_owned(),
        Message::FadeOut => "Fade out".to_owned(),
        Message::NoEffectsYet => "No effects yet. Add one in the Effects tab.".to_owned(),
        Message::Remove => "Remove".to_owned(),
        Message::Reset => "Default".to_owned(),
        Message::PackageParamsUnreadable => "The parameters of this package could not be read.".to_owned(),
        Message::Variables(names) => format!("Variables: {names}"),

        Message::Player => "Player".to_owned(),
        Message::Fullscreen => "Fullscreen".to_owned(),
        Message::ExitFullscreen => "Exit fullscreen".to_owned(),
        Message::PlayShortcut => "Play (Space)".to_owned(),
        Message::StopShortcut => "Stop (Space)".to_owned(),

        Message::UndoShortcut => "Undo (Ctrl+Z)".to_owned(),
        Message::RedoShortcut => "Redo (Ctrl+Y)".to_owned(),
        Message::SplitShortcut => "Split at the playhead (S)".to_owned(),
        Message::TrimBefore => "Trim before the playhead".to_owned(),
        Message::TrimAfter => "Trim after the playhead".to_owned(),
        Message::DeleteShortcut => "Delete clip (Delete)".to_owned(),
        Message::ZoomIn => "Zoom in".to_owned(),
        Message::ZoomOut => "Zoom out".to_owned(),
        Message::AddTrack => "+ Track".to_owned(),
        Message::AddVideoTrack => "Add video track".to_owned(),
        Message::AddAudioTrack => "Add audio track".to_owned(),
        Message::VideoTrackNumber(number) => format!("Video track {number}"),
        Message::AudioTrackNumber(number) => format!("Audio track {number}"),
        Message::AddClipFromFile => "Add a clip from a file".to_owned(),
        Message::LockTrack => "Lock the track".to_owned(),
        Message::UnlockTrack => "Unlock the track".to_owned(),
        Message::HideTrack => "Hide the track from the picture, the sound and the export".to_owned(),
        Message::ShowTrack => "Show the track".to_owned(),
        Message::TrackLockedNote => "This track is locked: its clips can be looked at, not changed".to_owned(),
        Message::SpeedBlockedByEndTransition => "A transition sits on the end of this clip: remove it to change the speed".to_owned(),
    }
}
