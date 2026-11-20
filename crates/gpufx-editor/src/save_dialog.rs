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
            custom.AddCheckButton(ZIP_ID, &HSTRING::from(spec.zip_label.as_str()), spec.initial.bundle && spec.initial.zip)?;
            follow_bundle(&custom, spec.initial.bundle)?;
            let events: windows::Win32::UI::Shell::IFileDialogEvents = ZipFollowsBundle.into();
            let cookie = dialog.Advise(&events)?;
            let parent = (spec.parent != 0).then(|| HWND(spec.parent as *mut _));
            let shown = dialog.Show(parent);
            let _ = dialog.Unadvise(cookie);
            if let Err(e) = shown {
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

#[cfg(windows)]
const BUNDLE_ID: u32 = 1;
#[cfg(windows)]
const ZIP_ID: u32 = 2;

/// "Pack as zip" only makes sense with the bundle: without it the button is greyed out and unchecked.
#[cfg(windows)]
fn follow_bundle(custom: &windows::Win32::UI::Shell::IFileDialogCustomize, bundle: bool) -> windows::core::Result<()> {
    use windows::Win32::UI::Shell::{CDCS_ENABLEDVISIBLE, CDCS_VISIBLE};
    // SAFETY: COM calls on the dialog's own thread.
    unsafe {
        if !bundle {
            custom.SetCheckButtonState(ZIP_ID, false)?;
        }
        custom.SetControlState(ZIP_ID, if bundle { CDCS_ENABLEDVISIBLE } else { CDCS_VISIBLE })
    }
}

/// Listens to the check buttons of the dialog so the zip one follows the bundle one.
#[cfg(windows)]
#[windows_core::implement(windows::Win32::UI::Shell::IFileDialogEvents, windows::Win32::UI::Shell::IFileDialogControlEvents)]
struct ZipFollowsBundle;

#[cfg(windows)]
mod events {
    use windows::core::{Ref, Result, BOOL};
    use windows::Win32::UI::Shell::*;

    use super::*;

    impl IFileDialogEvents_Impl for ZipFollowsBundle_Impl {
        fn OnFileOk(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }
        fn OnFolderChanging(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }
        fn OnFolderChange(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }
        fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }
        fn OnShareViolation(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> Result<FDE_SHAREVIOLATION_RESPONSE> {
            Ok(FDESVR_DEFAULT)
        }
        fn OnTypeChange(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }
        fn OnOverwrite(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> Result<FDE_OVERWRITE_RESPONSE> {
            Ok(FDEOR_DEFAULT)
        }
    }

    impl IFileDialogControlEvents_Impl for ZipFollowsBundle_Impl {
        fn OnItemSelected(&self, _: Ref<IFileDialogCustomize>, _: u32, _: u32) -> Result<()> {
            Ok(())
        }
        fn OnButtonClicked(&self, _: Ref<IFileDialogCustomize>, _: u32) -> Result<()> {
            Ok(())
        }
        fn OnCheckButtonToggled(&self, custom: Ref<IFileDialogCustomize>, control: u32, checked: BOOL) -> Result<()> {
            match (control, custom.as_ref()) {
                (BUNDLE_ID, Some(custom)) => follow_bundle(custom, checked.as_bool()),
                _ => Ok(()),
            }
        }
        fn OnControlActivating(&self, _: Ref<IFileDialogCustomize>, _: u32) -> Result<()> {
            Ok(())
        }
    }
}
