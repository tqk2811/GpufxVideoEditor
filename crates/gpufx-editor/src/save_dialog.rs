//! The Save dialog of the project: on Windows the shell's own `IFileSaveDialog` with two check
//! buttons (bundle the packs and media, pack into one file); elsewhere `rfd` without them.

use std::path::PathBuf;

use crate::tasks::SaveChoice;

/// What the dialog shows: the texts are already in the editor's language.
pub struct SaveDialogSpec {
    /// The parent window, as a raw `HWND` (0 for none).
    pub parent: isize,
    pub filter_name: String,
    /// The extension the filter accepts, without the dot or `*`.
    pub filter_extension: String,
    pub default_name: String,
    pub bundle_label: String,
    pub zip_label: String,
    /// The state the check buttons start in.
    pub initial: SaveChoice,
}

/// Shows the dialog and waits for it: the chosen path and check buttons, or `None` when cancelled.
/// Blocks, so it runs off the UI thread.
#[cfg(windows)]
pub fn show(spec: &SaveDialogSpec) -> Result<Option<(PathBuf, SaveChoice)>, String> {
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileSaveDialog, IFileDialogCustomize, IFileSaveDialog, FOS_OVERWRITEPROMPT, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};

    const BUNDLE_ID: u32 = 1;
    const ZIP_ID: u32 = 2;

    // SAFETY: plain COM calls on this thread, which initialises COM for itself and undoes it.
    unsafe {
        let initialised = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok();
        let result = (|| -> windows::core::Result<Option<(PathBuf, SaveChoice)>> {
            let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            let name = HSTRING::from(spec.filter_name.as_str());
            let pattern = HSTRING::from(format!("*.{}", spec.filter_extension));
            dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(pattern.as_ptr()) }])?;
            // No default extension: a double one is added by the caller, see `with_project_extension`.
            dialog.SetFileName(&HSTRING::from(spec.default_name.as_str()))?;
            dialog.SetOptions(dialog.GetOptions()? | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)?;
            let custom: IFileDialogCustomize = dialog.cast()?;
            custom.AddCheckButton(BUNDLE_ID, &HSTRING::from(spec.bundle_label.as_str()), spec.initial.bundle)?;
            custom.AddCheckButton(ZIP_ID, &HSTRING::from(spec.zip_label.as_str()), spec.initial.zip)?;
            let parent = (spec.parent != 0).then(|| HWND(spec.parent as *mut _));
            if let Err(e) = dialog.Show(parent) {
                if e.code() == ERROR_CANCELLED.to_hresult() {
                    return Ok(None);
                }
                return Err(e);
            }
            let item = dialog.GetResult()?;
            let raw = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = raw.to_string();
            CoTaskMemFree(Some(raw.0 as _));
            let choice = SaveChoice { bundle: custom.GetCheckButtonState(BUNDLE_ID)?.as_bool(), zip: custom.GetCheckButtonState(ZIP_ID)?.as_bool() };
            Ok(Some((PathBuf::from(path.map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?), choice)))
        })();
        if initialised {
            CoUninitialize();
        }
        result.map_err(|e| format!("the Save dialog failed: {e}"))
    }
}

/// Shows the dialog and waits for it: the chosen path, with no check buttons, or `None` when cancelled.
#[cfg(not(windows))]
pub fn show(spec: &SaveDialogSpec) -> Result<Option<(PathBuf, SaveChoice)>, String> {
    let path = rfd::FileDialog::new().add_filter(&spec.filter_name, &[spec.filter_extension.as_str()]).set_file_name(&spec.default_name).save_file();
    Ok(path.map(|path| (path, SaveChoice::default())))
}
