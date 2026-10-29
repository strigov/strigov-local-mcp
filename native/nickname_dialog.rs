use anyhow::Result;

#[cfg(target_os = "macos")]
pub fn prompt(current: &str) -> Result<Option<String>> {
    text(
        "Прозвище папки",
        "Название в меню и ChatGPT. Папка на диске не переименуется.",
        current,
        false,
    )
}
#[cfg(target_os = "macos")]
pub fn text(title: &str, description: &str, current: &str, secure: bool) -> Result<Option<String>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAlert, NSAlertFirstButtonReturn, NSApplication, NSSecureTextField, NSTextField,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("Dialog requires the main thread"))?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(description));
    alert.addButtonWithTitle(&NSString::from_str("Сохранить"));
    alert
        .addButtonWithTitle(&NSString::from_str("Отмена"))
        .setKeyEquivalent(&NSString::from_str("\u{1b}"));
    let ordinary = NSTextField::textFieldWithString(&NSString::from_str(current), mtm);
    let password = NSSecureTextField::new(mtm);
    let field: &NSTextField = if secure { &password } else { &ordinary };
    field.setStringValue(&NSString::from_str(current));
    field.setBezeled(true);
    field.setEditable(true);
    field.setSelectable(true);
    field.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(330.0, 24.0),
    ));

    alert.setAccessoryView(Some(field));
    alert.window().setInitialFirstResponder(Some(field));
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    let result = alert.runModal();
    Ok((result == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string()))
}

#[cfg(target_os = "windows")]
pub fn prompt(current: &str) -> Result<Option<String>> {
    text(
        "Прозвище папки",
        "Название в меню и ChatGPT (папка на диске не переименуется):",
        current,
        false,
    )
}

#[cfg(target_os = "windows")]
pub fn text(title: &str, description: &str, current: &str, secure: bool) -> Result<Option<String>> {
    windows::prompt(title, description, current, secure)
}
#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, RECT, WPARAM},
        Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Input::KeyboardAndMouse::SetFocus, WindowsAndMessaging::*},
    };
    struct State {
        initial: Vec<u16>,
        description: String,
        secure: bool,
        result: Option<String>,
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    // DialogBox owns the modal loop. The State reference remains alive until it returns.
    unsafe extern "system" fn dialog(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> isize {
        unsafe {
            if message == WM_INITDIALOG {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, l);
                let state = &*(l as *const State);
                for (class, text, style, id, rect) in [
                    (
                        "STATIC",
                        state.description.as_str(),
                        0,
                        10,
                        (10, 8, 240, 22),
                    ),
                    (
                        "EDIT",
                        "",
                        WS_BORDER
                            | WS_TABSTOP
                            | ES_AUTOHSCROLL as u32
                            | if state.secure { ES_PASSWORD as u32 } else { 0 },
                        1001,
                        (10, 33, 240, 16),
                    ),
                    (
                        "BUTTON",
                        "Сохранить",
                        WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                        IDOK,
                        (121, 62, 62, 18),
                    ),
                    (
                        "BUTTON",
                        "Отмена",
                        WS_TABSTOP | BS_PUSHBUTTON as u32,
                        IDCANCEL,
                        (190, 62, 60, 18),
                    ),
                ] {
                    let (x, y, width, height) = rect;
                    let mut r = RECT {
                        left: x,
                        top: y,
                        right: x + width,
                        bottom: y + height,
                    };
                    MapDialogRect(hwnd, &mut r);
                    let control = CreateWindowExW(
                        0,
                        wide(class).as_ptr(),
                        wide(text).as_ptr(),
                        WS_CHILD | WS_VISIBLE | style,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        hwnd,
                        id as usize as _,
                        GetModuleHandleW(null()),
                        null(),
                    );
                    if control.is_null() {
                        EndDialog(hwnd, -1);
                        return 0;
                    }
                    SendMessageW(
                        control,
                        WM_SETFONT,
                        GetStockObject(DEFAULT_GUI_FONT) as usize,
                        1,
                    );
                }
                SetDlgItemTextW(hwnd, 1001, state.initial.as_ptr());
                let field = GetDlgItem(hwnd, 1001);
                SendMessageW(field, 0x00B1 /* EM_SETSEL */, 0, -1);
                SetFocus(field);
                return 0;
            }
            if message == WM_COMMAND {
                let command = (w & 0xffff) as i32;
                if command == IDOK {
                    let field = GetDlgItem(hwnd, 1001);
                    let len = GetWindowTextLengthW(field).max(0) as usize;
                    let mut text = vec![0u16; len + 1];
                    let count = GetWindowTextW(field, text.as_mut_ptr(), text.len() as i32);
                    let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State);
                    state.result = Some(String::from_utf16_lossy(&text[..count.max(0) as usize]));
                    EndDialog(hwnd, 1);
                    return 1;
                }
                if command == IDCANCEL {
                    EndDialog(hwnd, 0);
                    return 1;
                }
            }
            if message == WM_CLOSE {
                EndDialog(hwnd, 0);
                return 1;
            }
            0
        }
    }
    pub fn prompt(
        title: &str,
        description: &str,
        current: &str,
        secure: bool,
    ) -> Result<Option<String>> {
        // WORD-packed DLGTEMPLATE, aligned to DWORD for Win32.
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME as u32 | DS_CENTER as u32;
        let mut words = vec![
            style as u16,
            (style >> 16) as u16,
            0,
            0,
            0,
            0,
            0,
            260,
            91,
            0,
            0,
        ];
        words.extend(wide(title));
        if !words.len().is_multiple_of(2) {
            words.push(0);
        }
        let aligned: Vec<u32> = words
            .as_chunks::<2>()
            .0
            .iter()
            .map(|w| w[0] as u32 | ((w[1] as u32) << 16))
            .collect();
        let mut state = State {
            initial: wide(current),
            description: description.into(),
            secure,
            result: None,
        };
        let outcome = unsafe {
            DialogBoxIndirectParamW(
                GetModuleHandleW(null()),
                aligned.as_ptr().cast(),
                null_mut(),
                Some(dialog),
                &mut state as *mut State as isize,
            )
        };
        anyhow::ensure!(outcome != -1, "Не удалось открыть окно переименования");
        Ok(state.result)
    }
}
