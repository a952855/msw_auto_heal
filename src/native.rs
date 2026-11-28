use crate::{
    config::{self, Profile, Settings},
    monitor::{MonitorEvent, Worker},
};
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::{collections::HashMap, ptr};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{Dialogs::*, *},
        HiDpi::*,
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::*,
    },
};

const ID_PREFIX: i32 = 100;
const ID_AUTO: i32 = 101;
const ID_PROFILE_TABS: i32 = 102;
const ID_PROFILE_NAME: i32 = 103;
const ID_PROFILE_RENAME: i32 = 104;
const ID_PROFILE_NEW: i32 = 105;
const ID_PROFILE_DELETE: i32 = 106;
const ID_POLL_INTERVAL: i32 = 107;
const ID_FAILURE_INTERVAL: i32 = 108;
const ID_HP_MAX: i32 = 150;
const ID_HP_DAMAGE: i32 = 151;
const ID_HP_SAFE: i32 = 152;
const ID_HP_BASE: i32 = 110;
const ID_MP_BASE: i32 = 120;
const ID_START_HP: i32 = 130;
const ID_START_MP: i32 = 131;
const ID_START_ALL: i32 = 132;
const ID_SAVE: i32 = 133;
const ID_PICK_HP: i32 = 134;
const ID_PICK_MP: i32 = 135;
const ID_IMPORT: i32 = 136;
const ID_EXPORT: i32 = 137;
const ID_HP_STATUS: i32 = 140;
const ID_MP_STATUS: i32 = 141;
const ID_HP_SCAN: i32 = 142;
const ID_MP_SCAN: i32 = 143;
const TIMER_EVENTS: usize = 1;
const HOTKEY_ALL: i32 = 1;
const HOTKEY_HP: i32 = 2;
const HOTKEY_MP: i32 = 3;
const WS_CHILD_VISIBLE: u32 = WS_CHILD | WS_VISIBLE;

struct AppState {
    hwnd: HWND,
    font: HFONT,
    controls: HashMap<i32, HWND>,
    settings: Settings,
    config_valid: bool,
    path: std::path::PathBuf,
    workers: [Option<Worker>; 2],
    tx: Sender<MonitorEvent>,
    rx: Receiver<MonitorEvent>,
    loading_controls: bool,
    capturing_key: Option<bool>,
}

static mut APP: *mut AppState = ptr::null_mut();
static mut SELECT_STATE: *mut SelectionState = ptr::null_mut();
static mut SELECT_CLASS_READY: bool = false;
static mut REENTRANT_GUARD: bool = false;
struct SelectionState {
    result: Option<[i32; 4]>,
    start: [i32; 2],
    current: [i32; 2],
    origin: [i32; 2],
}

struct ReentrantGuard;
impl ReentrantGuard {
    unsafe fn new() -> Self {
        REENTRANT_GUARD = true;
        Self
    }
}
impl Drop for ReentrantGuard {
    fn drop(&mut self) {
        unsafe {
            REENTRANT_GUARD = false;
        }
    }
}

pub fn run() -> Result<(), String> {
    add_tesseract_to_path();
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        InitCommonControls();
    }
    let path = config::settings_path().map_err(|e| e.to_string())?;
    let loaded = config::load(&path);
    let (settings, load_error, config_valid) = match loaded {
        Ok(s) => (s, None, true),
        Err(e) => (Settings::default(), Some(e.to_string()), false),
    };
    let (tx, rx) = unbounded();
    let mut app = Box::new(AppState {
        hwnd: ptr::null_mut(),
        font: ptr::null_mut(),
        controls: HashMap::new(),
        settings,
        config_valid,
        path,
        workers: [None, None],
        tx,
        rx,
        loading_controls: true,
        capturing_key: None,
    });
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let class = wide("MswAutoHealNativeWindow");
    let title = wide("Msw 補血補魔器");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
        hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        if RegisterClassW(&wc) == 0 {
            return Err("無法註冊 Windows 視窗類別".into());
        }
        APP = &mut *app;
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            900,
            710,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        if hwnd.is_null() {
            APP = ptr::null_mut();
            return Err("無法建立主視窗".into());
        }
        app.hwnd = hwnd;
        let face = wide("Microsoft JhengHei");
        app.font = CreateFontW(
            -15,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            face.as_ptr(),
        );
        create_controls(&mut app);
        refresh_profile_tabs(&app, app.settings.active_profile);
        if !app.font.is_null() {
            EnumChildWindows(app.hwnd, Some(apply_child_font), app.font as LPARAM);
        }
        load_controls(&app);
        app.loading_controls = false;
        let failed = [(HOTKEY_ALL, VK_F5), (HOTKEY_HP, VK_F6), (HOTKEY_MP, VK_F7)]
            .into_iter()
            .filter_map(|(id, key)| {
                (RegisterHotKey(hwnd, id, MOD_NOREPEAT, key as u32) == 0)
                    .then_some(format!("F{}", key - VK_F1 + 1))
            })
            .collect::<Vec<_>>();
        SetTimer(hwnd, TIMER_EVENTS, 100, None);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        if !failed.is_empty() {
            set_text(
                app.controls[&ID_HP_STATUS],
                &format!(
                    "以下全域快捷鍵無法註冊，可能已被其他程式占用：{}",
                    failed.join("、")
                ),
            );
        }
        if let Some(error) = load_error {
            set_text(
                app.controls[&ID_HP_STATUS],
                &format!("設定檔讀取失敗，已使用預設值，原檔未修改。\n{error}"),
            );
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn add_tesseract_to_path() {
    const TESSERACT_DIR: &str = r"C:\Program Files\Tesseract-OCR";
    let mut paths =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    if paths
        .iter()
        .any(|path| path.to_string_lossy().eq_ignore_ascii_case(TESSERACT_DIR))
    {
        return;
    }
    paths.push(TESSERACT_DIR.into());
    if let Ok(path) = std::env::join_paths(paths) {
        std::env::set_var("PATH", path);
    }
}

pub fn report_startup_error(message: &str) {
    unsafe {
        let title = wide("Msw 補血補魔器");
        let message = wide(message);
        MessageBoxW(
            ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if REENTRANT_GUARD {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let Some(app) = APP.as_mut() else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_COMMAND => {
            let id = (wparam & 0xffff) as i32;
            if id == ID_HP_BASE + 2 || id == ID_MP_BASE + 2 {
                app.capturing_key = Some(id == ID_MP_BASE + 2);
                set_text(app.controls[&id], "請按一個按鍵…（Esc 取消）");
                // SetFocus synchronously sends focus messages. Keep those nested
                // window-procedure calls out of the mutable APP borrow above.
                {
                    let _guard = ReentrantGuard::new();
                    SetFocus(app.hwnd);
                }
                return 0;
            }
            match id {
                ID_PROFILE_RENAME => rename_active_profile(app),
                ID_PROFILE_NEW => add_profile(app),
                ID_PROFILE_DELETE => delete_active_profile(app),
                ID_POLL_INTERVAL | ID_FAILURE_INTERVAL => {
                    if !app.loading_controls && !app.workers.iter().any(Option::is_some) {
                        let _ = save_from_controls(app, false, false);
                    }
                }
                ID_HP_MAX | ID_HP_DAMAGE => {
                    if !app.loading_controls {
                        update_hp_safety_threshold(app);
                    }
                }
                ID_SAVE => {
                    let _ = save_from_controls(app, true, true);
                }
                ID_PICK_HP => pick_region(app, false),
                ID_PICK_MP => pick_region(app, true),
                ID_IMPORT => import_settings(app),
                ID_EXPORT => export_settings(app),
                ID_START_HP => toggle_worker(app, false),
                ID_START_MP => toggle_worker(app, true),
                ID_START_ALL => {
                    let any = app.workers.iter().any(Option::is_some);
                    if any {
                        stop_all(app);
                    } else {
                        start_all(app);
                    }
                }
                ID_AUTO => {
                    if !app.loading_controls {
                        let _ = save_from_controls(app, false, false);
                    }
                }
                _ if (ID_HP_BASE..ID_HP_BASE + 9).contains(&id)
                    || (ID_MP_BASE..ID_MP_BASE + 9).contains(&id) =>
                {
                    if !app.loading_controls {
                        let _ = save_from_controls(app, false, false);
                    }
                }
                _ => {}
            }
            0
        }
        WM_NOTIFY if lparam != 0 => {
            let notification = &*(lparam as *const NMHDR);
            if notification.idFrom == ID_PROFILE_TABS as usize {
                match notification.code {
                    TCN_SELCHANGING => {
                        if app.workers.iter().any(Option::is_some) {
                            set_error(app, "監控執行中，請先停止 HP／MP 才能切換設定檔。");
                            return 1;
                        }
                        if app.capturing_key.is_some() || !save_from_controls(app, false, false) {
                            return 1;
                        }
                    }
                    TCN_SELCHANGE => {
                        let selected =
                            SendMessageW(app.controls[&ID_PROFILE_TABS], TCM_GETCURSEL, 0, 0);
                        if selected >= 0 {
                            activate_profile(app, selected as usize);
                        }
                    }
                    _ => {}
                }
            }
            0
        }
        WM_HOTKEY => {
            match wparam as i32 {
                HOTKEY_ALL => send_command(app, ID_START_ALL),
                HOTKEY_HP => send_command(app, ID_START_HP),
                HOTKEY_MP => send_command(app, ID_START_MP),
                _ => {}
            }
            0
        }
        WM_KEYDOWN if app.capturing_key.is_some() => {
            let mana = app.capturing_key.take().unwrap();
            if wparam as u32 == VK_ESCAPE as u32 {
                load_controls(app);
            } else if let Some(key) = key_name(wparam as u32) {
                let id = if mana { ID_MP_BASE + 2 } else { ID_HP_BASE + 2 };
                set_text(app.controls[&id], &key);
                let _ = save_from_controls(app, false, false);
            } else {
                let id = if mana { ID_MP_BASE + 2 } else { ID_HP_BASE + 2 };
                set_text(app.controls[&id], "不支援此按鍵，請重試");
                app.capturing_key = Some(mana);
            }
            0
        }
        WM_TIMER if wparam == TIMER_EVENTS => {
            drain_events(app);
            0
        }
        WM_CLOSE => {
            stop_all(app);
            let _ = save_from_controls(app, false, false);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            UnregisterHotKey(hwnd, HOTKEY_ALL);
            UnregisterHotKey(hwnd, HOTKEY_HP);
            UnregisterHotKey(hwnd, HOTKEY_MP);
            KillTimer(hwnd, TIMER_EVENTS);
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => {
            if !app.font.is_null() {
                DeleteObject(app.font as HGDIOBJ);
                app.font = ptr::null_mut();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn apply_child_font(hwnd: HWND, font: LPARAM) -> i32 {
    SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
    1
}

unsafe fn create_controls(app: &mut AppState) {
    app.controls.insert(
        ID_PREFIX,
        control(
            app.hwnd,
            "EDIT",
            "Maple",
            WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
            140,
            14,
            210,
            25,
            ID_PREFIX,
        ),
    );
    control(
        app.hwnd,
        "STATIC",
        "視窗標題前綴",
        WS_CHILD_VISIBLE,
        20,
        18,
        110,
        20,
        0,
    );
    app.controls.insert(
        ID_AUTO,
        control(
            app.hwnd,
            "BUTTON",
            "自動偵測 HP / MP 標籤位置",
            WS_CHILD_VISIBLE | (BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            360,
            14,
            220,
            26,
            ID_AUTO,
        ),
    );
    control(
        app.hwnd,
        "STATIC",
        "循環(ms)",
        WS_CHILD_VISIBLE,
        590,
        18,
        72,
        20,
        0,
    );
    app.controls.insert(
        ID_POLL_INTERVAL,
        control(
            app.hwnd,
            "EDIT",
            "80",
            WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
            665,
            14,
            60,
            25,
            ID_POLL_INTERVAL,
        ),
    );
    control(
        app.hwnd,
        "STATIC",
        "失敗(ms)",
        WS_CHILD_VISIBLE,
        738,
        18,
        72,
        20,
        0,
    );
    app.controls.insert(
        ID_FAILURE_INTERVAL,
        control(
            app.hwnd,
            "EDIT",
            "80",
            WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
            815,
            14,
            55,
            25,
            ID_FAILURE_INTERVAL,
        ),
    );
    app.controls.insert(
        ID_PROFILE_TABS,
        control(
            app.hwnd,
            "SysTabControl32",
            "",
            WS_CHILD_VISIBLE | WS_TABSTOP | TCS_TABS,
            20,
            45,
            425,
            30,
            ID_PROFILE_TABS,
        ),
    );
    app.controls.insert(
        ID_PROFILE_NAME,
        control(
            app.hwnd,
            "EDIT",
            "",
            WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
            493,
            48,
            128,
            24,
            ID_PROFILE_NAME,
        ),
    );
    app.controls.insert(
        ID_PROFILE_RENAME,
        control(
            app.hwnd,
            "BUTTON",
            "更名",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            626,
            45,
            64,
            28,
            ID_PROFILE_RENAME,
        ),
    );
    app.controls.insert(
        ID_PROFILE_NEW,
        control(
            app.hwnd,
            "BUTTON",
            "新增",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            695,
            45,
            64,
            28,
            ID_PROFILE_NEW,
        ),
    );
    app.controls.insert(
        ID_PROFILE_DELETE,
        control(
            app.hwnd,
            "BUTTON",
            "刪除",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            764,
            45,
            64,
            28,
            ID_PROFILE_DELETE,
        ),
    );
    label(app.hwnd, "名稱", 455, 51);
    let auto = if app.settings.profiles[app.settings.active_profile]
        .ocr
        .auto_region
    {
        BST_CHECKED
    } else {
        BST_UNCHECKED
    };
    SendMessageW(app.controls[&ID_AUTO], BM_SETCHECK, auto as usize, 0);
    group(app.hwnd, "HP 補血", 20, 82, 410, 440);
    group(app.hwnd, "MP 補魔", 450, 82, 410, 440);
    for (base, x, mana) in [(ID_HP_BASE, 40, false), (ID_MP_BASE, 470, true)] {
        label(app.hwnd, "低於門檻時補水 (%)", x, 115);
        app.controls.insert(
            base,
            control(
                app.hwnd,
                "EDIT",
                "",
                WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
                x + 170,
                111,
                90,
                24,
                base,
            ),
        );
        let (key_label_y, key_input_y, region_label_y, region_input_y, pick_y) = if mana {
            (150, 146, 185, 212, 244)
        } else {
            for (id, title, y) in [
                (ID_HP_MAX, "角色 HP 上限", 150),
                (ID_HP_DAMAGE, "怪物最大傷害", 185),
            ] {
                label(app.hwnd, title, x, y);
                app.controls.insert(
                    id,
                    control(
                        app.hwnd,
                        "EDIT",
                        "0",
                        WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
                        x + 170,
                        y - 4,
                        100,
                        24,
                        id,
                    ),
                );
            }
            app.controls.insert(
                ID_HP_SAFE,
                control(
                    app.hwnd,
                    "STATIC",
                    "建議門檻：請輸入 HP 與傷害",
                    WS_CHILD_VISIBLE,
                    x + 4,
                    213,
                    360,
                    22,
                    ID_HP_SAFE,
                ),
            );
            (242, 238, 277, 304, 336)
        };
        label(app.hwnd, "補水按鍵", x, key_label_y);
        app.controls.insert(
            base + 2,
            control(
                app.hwnd,
                "BUTTON",
                "",
                WS_CHILD_VISIBLE | WS_TABSTOP,
                x + 170,
                key_input_y,
                120,
                24,
                base + 2,
            ),
        );
        label(app.hwnd, "手動區域 [左, 上, 右, 下]", x, region_label_y);
        for j in 0..4 {
            app.controls.insert(
                base + 4 + j,
                control(
                    app.hwnd,
                    "EDIT",
                    "",
                    WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
                    x + 4 + (j as i32) * 78,
                    region_input_y,
                    70,
                    24,
                    base + 4 + j,
                ),
            );
        }
        let pick_id = if mana { ID_PICK_MP } else { ID_PICK_HP };
        app.controls.insert(
            pick_id,
            control(
                app.hwnd,
                "BUTTON",
                "拖曳選取區域",
                WS_CHILD_VISIBLE | WS_TABSTOP,
                x + 4,
                pick_y,
                145,
                28,
                pick_id,
            ),
        );
        let _ = mana;
    }
    app.controls.insert(
        ID_HP_STATUS,
        control(
            app.hwnd,
            "STATIC",
            "尚未啟動",
            WS_CHILD_VISIBLE,
            40,
            390,
            370,
            55,
            ID_HP_STATUS,
        ),
    );
    app.controls.insert(
        ID_HP_SCAN,
        control(
            app.hwnd,
            "STATIC",
            "OCR：—",
            WS_CHILD_VISIBLE,
            40,
            450,
            370,
            60,
            ID_HP_SCAN,
        ),
    );
    app.controls.insert(
        ID_MP_STATUS,
        control(
            app.hwnd,
            "STATIC",
            "尚未啟動",
            WS_CHILD_VISIBLE,
            470,
            390,
            370,
            55,
            ID_MP_STATUS,
        ),
    );
    app.controls.insert(
        ID_MP_SCAN,
        control(
            app.hwnd,
            "STATIC",
            "OCR：—",
            WS_CHILD_VISIBLE,
            470,
            450,
            370,
            60,
            ID_MP_SCAN,
        ),
    );
    app.controls.insert(
        ID_START_HP,
        control(
            app.hwnd,
            "BUTTON",
            "F6 啟動 HP",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            20,
            532,
            170,
            38,
            ID_START_HP,
        ),
    );
    app.controls.insert(
        ID_START_MP,
        control(
            app.hwnd,
            "BUTTON",
            "F7 啟動 MP",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            200,
            532,
            170,
            38,
            ID_START_MP,
        ),
    );
    app.controls.insert(
        ID_START_ALL,
        control(
            app.hwnd,
            "BUTTON",
            "F5 啟動全部",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            380,
            532,
            190,
            38,
            ID_START_ALL,
        ),
    );
    app.controls.insert(
        ID_IMPORT,
        control(
            app.hwnd,
            "BUTTON",
            "匯入 JSON",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            580,
            532,
            90,
            38,
            ID_IMPORT,
        ),
    );
    app.controls.insert(
        ID_EXPORT,
        control(
            app.hwnd,
            "BUTTON",
            "匯出 JSON",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            680,
            532,
            90,
            38,
            ID_EXPORT,
        ),
    );
    app.controls.insert(
        ID_SAVE,
        control(
            app.hwnd,
            "BUTTON",
            "儲存設定",
            WS_CHILD_VISIBLE | WS_TABSTOP,
            780,
            532,
            90,
            38,
            ID_SAVE,
        ),
    );
    control(
        app.hwnd,
        "STATIC",
        "設定儲存於程式旁的 usersetting.json；Tesseract OCR 需安裝並可由 PATH 找到。",
        WS_CHILD_VISIBLE,
        25,
        587,
        840,
        60,
        0,
    );
}

fn key_name(vk: u32) -> Option<String> {
    if (0x30..=0x39).contains(&vk) || (0x41..=0x5a).contains(&vk) {
        return Some(char::from_u32(vk)?.to_string());
    }
    if (0x70..=0x7b).contains(&vk) {
        return Some(format!("F{}", vk - 0x70 + 1));
    }
    Some(
        match vk {
            0x10 => "Shift",
            0x11 => "Ctrl",
            0x12 => "Alt",
            0x2e => "Delete",
            0x23 => "End",
            0x24 => "Home",
            0x2d => "Insert",
            0x21 => "PageUp",
            0x22 => "PageDown",
            0x20 => "Space",
            0x0d => "Enter",
            0x09 => "Tab",
            0x1b => "Esc",
            0x26 => "Up",
            0x28 => "Down",
            0x25 => "Left",
            0x27 => "Right",
            0x08 => "Backspace",
            0xbd => "-",
            0xbb => "=",
            0xdb => "[",
            0xdd => "]",
            0xba => ";",
            0xde => "'",
            0xbc => ",",
            0xbe => ".",
            0xbf => "/",
            _ => return None,
        }
        .to_string(),
    )
}

unsafe fn load_controls(app: &AppState) {
    let _guard = ReentrantGuard::new();
    let profile = &app.settings.profiles[app.settings.active_profile];
    let s = &profile.ocr;
    SendMessageW(
        app.controls[&ID_AUTO],
        BM_SETCHECK,
        if s.auto_region {
            BST_CHECKED as usize
        } else {
            BST_UNCHECKED as usize
        },
        0,
    );
    for (id, value) in [
        (ID_PREFIX, app.settings.window_prefix.clone()),
        (ID_POLL_INTERVAL, app.settings.poll_interval_ms.to_string()),
        (
            ID_FAILURE_INTERVAL,
            app.settings.failure_interval_ms.to_string(),
        ),
        (ID_PROFILE_NAME, profile.name.clone()),
        (ID_HP_BASE, s.th_var.to_string()),
        (ID_HP_MAX, s.hp_max.to_string()),
        (ID_HP_DAMAGE, s.monster_max_damage.to_string()),
        (ID_HP_BASE + 2, s.key_var.clone()),
        (ID_MP_BASE, s.mana_th_var.to_string()),
        (ID_MP_BASE + 2, s.mana_key_var.clone()),
    ] {
        set_text(app.controls[&id], &value);
    }
    for (base, region) in [(ID_HP_BASE, s.region), (ID_MP_BASE, s.mana_region)] {
        for (j, value) in region.iter().enumerate() {
            set_text(app.controls[&(base + 4 + j as i32)], &value.to_string());
        }
    }
    update_hp_suggestion_label(app, s.hp_max, s.monster_max_damage);
}

unsafe fn save_from_controls(
    app: &mut AppState,
    show: bool,
    allow_invalid_replacement: bool,
) -> bool {
    let active_profile = app.settings.active_profile;
    let mut s = app.settings.profiles[active_profile].ocr.clone();
    let window_prefix = get_text(app.controls[&ID_PREFIX]);
    let poll_interval_ms = get_text(app.controls[&ID_POLL_INTERVAL]).parse::<u64>();
    let failure_interval_ms = get_text(app.controls[&ID_FAILURE_INTERVAL]).parse::<u64>();
    let (Ok(poll_interval_ms), Ok(failure_interval_ms)) = (poll_interval_ms, failure_interval_ms)
    else {
        if show {
            set_error(app, "循環與失敗間隔必須是整數毫秒。");
        }
        return false;
    };
    if !(10..=5000).contains(&poll_interval_ms) || !(10..=5000).contains(&failure_interval_ms) {
        if show {
            set_error(app, "循環與失敗間隔需介於 10 到 5000 毫秒。");
        }
        return false;
    }
    let hp_max = get_text(app.controls[&ID_HP_MAX]).parse::<i32>();
    let monster_max_damage = get_text(app.controls[&ID_HP_DAMAGE]).parse::<i32>();
    let (Ok(hp_max), Ok(monster_max_damage)) = (hp_max, monster_max_damage) else {
        if show {
            set_error(app, "角色 HP 上限與怪物最大傷害必須是整數。");
        }
        return false;
    };
    if hp_max < 0 || monster_max_damage < 0 {
        if show {
            set_error(app, "角色 HP 上限與怪物最大傷害不可為負數。");
        }
        return false;
    }
    s.hp_max = hp_max;
    s.monster_max_damage = monster_max_damage;
    s.auto_region = SendMessageW(app.controls[&ID_AUTO], BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
    let values = [(ID_HP_BASE, false), (ID_MP_BASE, true)];
    for (base, mana) in values {
        let threshold = get_text(app.controls[&base]).parse::<f64>();
        let key = get_text(app.controls[&(base + 2)]);
        let Ok(threshold) = threshold else {
            if show {
                set_error(app, "補水門檻必須是有效數字。");
            }
            return false;
        };
        if mana {
            s.mana_th_var = threshold;
            s.mana_key_var = key;
        } else {
            s.th_var = threshold;
            s.key_var = key;
        }
        let mut region = [0; 4];
        for (j, slot) in region.iter_mut().enumerate() {
            let Ok(value) = get_text(app.controls[&(base + 4 + j as i32)]).parse::<i32>() else {
                if show {
                    set_error(app, "擷取區域需要四個整數座標。");
                }
                return false;
            };
            *slot = value;
        }
        if mana {
            s.mana_region = region;
        } else {
            s.region = region;
        }
    }
    if show {
        for mana in [false, true] {
            if let Err(error) = s.validate(mana) {
                set_error(app, &error);
                return false;
            }
        }
    }
    app.settings.window_prefix = window_prefix;
    app.settings.poll_interval_ms = poll_interval_ms;
    app.settings.failure_interval_ms = failure_interval_ms;
    app.settings.profiles[active_profile].ocr = s;
    if !app.config_valid && !allow_invalid_replacement {
        return true;
    }
    match config::save(&app.path, &app.settings) {
        Ok(()) => {
            app.config_valid = true;
            true
        }
        Err(error) => {
            if show {
                set_error(app, &format!("設定儲存失敗：{error}"));
            }
            false
        }
    }
}

unsafe fn update_hp_safety_threshold(app: &mut AppState) {
    let hp_max = get_text(app.controls[&ID_HP_MAX]).parse::<i32>();
    let max_damage = get_text(app.controls[&ID_HP_DAMAGE]).parse::<i32>();
    let (Ok(hp_max), Ok(max_damage)) = (hp_max, max_damage) else {
        set_text(app.controls[&ID_HP_SAFE], "建議門檻：請輸入有效整數");
        return;
    };
    if hp_max <= 0 || max_damage <= 0 {
        set_text(app.controls[&ID_HP_SAFE], "建議門檻：HP 與傷害都需大於 0");
        return;
    }
    let threshold = (((max_damage as f64 / hp_max as f64) * 1000.0).ceil() / 10.0).min(100.0);
    {
        let _guard = ReentrantGuard::new();
        set_text(app.controls[&ID_HP_BASE], &format!("{threshold:.1}"));
    }
    update_hp_suggestion_label(app, hp_max, max_damage);
    let _ = save_from_controls(app, false, false);
}

unsafe fn update_hp_suggestion_label(app: &AppState, hp_max: i32, max_damage: i32) {
    if hp_max <= 0 || max_damage <= 0 {
        set_text(app.controls[&ID_HP_SAFE], "建議門檻：HP 與傷害都需大於 0");
        return;
    }
    let threshold = (((max_damage as f64 / hp_max as f64) * 1000.0).ceil() / 10.0).min(100.0);
    if max_damage >= hp_max {
        set_text(
            app.controls[&ID_HP_SAFE],
            &format!("建議門檻：{threshold:.1}%（最大傷害可能秒殺）"),
        );
    } else {
        set_text(
            app.controls[&ID_HP_SAFE],
            &format!("建議門檻：{threshold:.1}%"),
        );
    }
}

unsafe fn toggle_worker(app: &mut AppState, mana: bool) {
    let index = usize::from(mana);
    if let Some(mut worker) = app.workers[index].take() {
        worker.stop();
        let status = if mana { ID_MP_STATUS } else { ID_HP_STATUS };
        set_text(app.controls[&status], "已停止");
        update_worker_buttons(app);
        return;
    }
    if !save_from_controls(app, true, false) {
        return;
    }
    let ocr = &app.settings.profiles[app.settings.active_profile].ocr;
    if let Err(error) = ocr.validate(mana) {
        set_error(app, &error);
        return;
    }
    app.workers[index] = Some(Worker::start(
        mana,
        ocr.clone(),
        app.settings.window_prefix.clone(),
        app.settings.poll_interval_ms,
        app.settings.failure_interval_ms,
        app.tx.clone(),
    ));
    let status = if mana { ID_MP_STATUS } else { ID_HP_STATUS };
    set_text(app.controls[&status], "啟動中…");
    update_worker_buttons(app);
}

unsafe fn stop_all(app: &mut AppState) {
    for i in 0..2 {
        if let Some(mut worker) = app.workers[i].take() {
            worker.stop();
        }
    }
    set_text(app.controls[&ID_HP_STATUS], "已停止");
    set_text(app.controls[&ID_MP_STATUS], "已停止");
    update_worker_buttons(app);
}

unsafe fn start_all(app: &mut AppState) {
    if !save_from_controls(app, true, false) {
        return;
    }
    for mana in [false, true] {
        if let Err(error) = app.settings.profiles[app.settings.active_profile]
            .ocr
            .validate(mana)
        {
            set_error(app, &error);
            return;
        }
    }
    app.workers[0] = Some(Worker::start(
        false,
        app.settings.profiles[app.settings.active_profile]
            .ocr
            .clone(),
        app.settings.window_prefix.clone(),
        app.settings.poll_interval_ms,
        app.settings.failure_interval_ms,
        app.tx.clone(),
    ));
    app.workers[1] = Some(Worker::start(
        true,
        app.settings.profiles[app.settings.active_profile]
            .ocr
            .clone(),
        app.settings.window_prefix.clone(),
        app.settings.poll_interval_ms,
        app.settings.failure_interval_ms,
        app.tx.clone(),
    ));
    set_text(app.controls[&ID_HP_STATUS], "啟動中…");
    set_text(app.controls[&ID_MP_STATUS], "啟動中…");
    update_worker_buttons(app);
}

unsafe fn update_worker_buttons(app: &AppState) {
    let hp_running = app.workers[0].is_some();
    let mp_running = app.workers[1].is_some();
    let timing_enabled = !hp_running && !mp_running;
    EnableWindow(app.controls[&ID_POLL_INTERVAL], timing_enabled as BOOL);
    EnableWindow(app.controls[&ID_FAILURE_INTERVAL], timing_enabled as BOOL);
    set_text(
        app.controls[&ID_START_HP],
        if hp_running {
            "F6 停止 HP（執行中）"
        } else {
            "F6 啟動 HP"
        },
    );
    set_text(
        app.controls[&ID_START_MP],
        if mp_running {
            "F7 停止 MP（執行中）"
        } else {
            "F7 啟動 MP"
        },
    );
    set_text(
        app.controls[&ID_START_ALL],
        if hp_running || mp_running {
            "F5 停止全部"
        } else {
            "F5 啟動全部"
        },
    );
}

unsafe fn refresh_profile_tabs(app: &AppState, selected: usize) {
    let tabs = app.controls[&ID_PROFILE_TABS];
    let count = SendMessageW(tabs, TCM_GETITEMCOUNT, 0, 0).max(0);
    for _ in 0..count {
        SendMessageW(tabs, TCM_DELETEITEM, 0, 0);
    }
    for (index, profile) in app.settings.profiles.iter().enumerate() {
        let mut title = wide(&profile.name);
        let mut item: TCITEMW = std::mem::zeroed();
        item.mask = TCIF_TEXT;
        item.pszText = title.as_mut_ptr();
        item.cchTextMax = title.len() as i32;
        SendMessageW(
            tabs,
            TCM_INSERTITEMW,
            index,
            &item as *const TCITEMW as LPARAM,
        );
    }
    if !app.settings.profiles.is_empty() {
        SendMessageW(
            tabs,
            TCM_SETCURSEL,
            selected.min(app.settings.profiles.len() - 1),
            0,
        );
    }
}

unsafe fn activate_profile(app: &mut AppState, index: usize) {
    if index >= app.settings.profiles.len() || index == app.settings.active_profile {
        return;
    }
    app.settings.active_profile = index;
    app.loading_controls = true;
    load_controls(app);
    app.loading_controls = false;
    if app.config_valid {
        if let Err(error) = config::save(&app.path, &app.settings) {
            set_error(app, &format!("目前設定檔已切換，但索引儲存失敗：{error}"));
            return;
        }
    }
    set_text(
        app.controls[&ID_HP_STATUS],
        &format!("已切換設定檔：{}", app.settings.profiles[index].name),
    );
}

unsafe fn profile_changes_allowed(app: &AppState) -> bool {
    if app.workers.iter().any(Option::is_some) {
        set_error(app, "監控執行中，請先停止 HP／MP 再管理設定檔。");
        false
    } else {
        true
    }
}

unsafe fn rename_active_profile(app: &mut AppState) {
    if !profile_changes_allowed(app) || !save_from_controls(app, false, false) {
        return;
    }
    let name = get_text(app.controls[&ID_PROFILE_NAME]).trim().to_owned();
    if name.is_empty() {
        set_error(app, "設定檔名稱不可空白。");
        return;
    }
    if app
        .settings
        .profiles
        .iter()
        .enumerate()
        .any(|(index, profile)| {
            index != app.settings.active_profile && profile.name.eq_ignore_ascii_case(&name)
        })
    {
        set_error(app, "設定檔名稱不可重複。");
        return;
    }
    let index = app.settings.active_profile;
    let previous = app.settings.profiles[index].name.clone();
    app.settings.profiles[index].name = name;
    match config::save(&app.path, &app.settings) {
        Ok(()) => {
            app.config_valid = true;
            refresh_profile_tabs(app, index);
            load_controls(app);
            set_text(app.controls[&ID_HP_STATUS], "設定檔名稱已更新。");
        }
        Err(error) => {
            app.settings.profiles[index].name = previous;
            load_controls(app);
            set_error(app, &format!("設定檔名稱儲存失敗：{error}"));
        }
    }
}

unsafe fn add_profile(app: &mut AppState) {
    if !profile_changes_allowed(app) || !save_from_controls(app, false, false) {
        return;
    }
    let mut number = app.settings.profiles.len() + 1;
    let name = loop {
        let candidate = format!("角色 {number}");
        if app
            .settings
            .profiles
            .iter()
            .all(|profile| profile.name != candidate)
        {
            break candidate;
        }
        number += 1;
    };
    let source = app.settings.profiles[app.settings.active_profile]
        .ocr
        .clone();
    app.settings.profiles.push(Profile { name, ocr: source });
    app.settings.active_profile = app.settings.profiles.len() - 1;
    match config::save(&app.path, &app.settings) {
        Ok(()) => {
            app.config_valid = true;
            refresh_profile_tabs(app, app.settings.active_profile);
            app.loading_controls = true;
            load_controls(app);
            app.loading_controls = false;
            set_text(
                app.controls[&ID_HP_STATUS],
                "已新增設定檔，可修改名稱與角色設定。",
            );
        }
        Err(error) => {
            app.settings.profiles.pop();
            app.settings.active_profile = app.settings.profiles.len() - 1;
            refresh_profile_tabs(app, app.settings.active_profile);
            load_controls(app);
            set_error(app, &format!("新增設定檔儲存失敗：{error}"));
        }
    }
}

unsafe fn delete_active_profile(app: &mut AppState) {
    if !profile_changes_allowed(app) || !save_from_controls(app, false, false) {
        return;
    }
    if app.settings.profiles.len() <= 1 {
        set_error(app, "至少要保留一個設定檔。");
        return;
    }
    let name = &app.settings.profiles[app.settings.active_profile].name;
    let prompt = wide(&format!("確定刪除設定檔「{name}」？此操作無法復原。"));
    let title = wide("刪除設定檔");
    let confirmed = {
        let _guard = ReentrantGuard::new();
        MessageBoxW(
            app.hwnd,
            prompt.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING,
        ) == IDYES
    };
    if !confirmed {
        return;
    }
    let previous = app.settings.clone();
    app.settings.profiles.remove(app.settings.active_profile);
    app.settings.active_profile = app
        .settings
        .active_profile
        .min(app.settings.profiles.len() - 1);
    match config::save(&app.path, &app.settings) {
        Ok(()) => {
            app.config_valid = true;
            refresh_profile_tabs(app, app.settings.active_profile);
            app.loading_controls = true;
            load_controls(app);
            app.loading_controls = false;
            set_text(app.controls[&ID_HP_STATUS], "設定檔已刪除。");
        }
        Err(error) => {
            app.settings = previous;
            refresh_profile_tabs(app, app.settings.active_profile);
            load_controls(app);
            set_error(app, &format!("刪除設定檔儲存失敗：{error}"));
        }
    }
}
unsafe fn send_command(app: &AppState, id: i32) {
    let _ = PostMessageW(app.hwnd, WM_COMMAND, id as usize, 0);
}
unsafe fn drain_events(app: &mut AppState) {
    while let Ok(event) = app.rx.try_recv() {
        let (status, scan) = if event.mana {
            (ID_MP_STATUS, ID_MP_SCAN)
        } else {
            (ID_HP_STATUS, ID_HP_SCAN)
        };
        set_text(app.controls[&status], &event.text);
        if let Some(text) = event.scan {
            set_text(app.controls[&scan], &format!("OCR：{text}"));
        }
    }
}

unsafe fn pick_region(app: &mut AppState, mana: bool) {
    let _guard = ReentrantGuard::new();
    let instance = GetModuleHandleW(ptr::null());
    let class = wide("MswAutoHealRegionOverlay");
    if !SELECT_CLASS_READY {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(select_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_CROSS),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        SELECT_CLASS_READY = true;
    }
    let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    let mut selection = SelectionState {
        result: None,
        start: [0, 0],
        current: [0, 0],
        origin: [x, y],
    };
    SELECT_STATE = &mut selection;
    ShowWindow(app.hwnd, SW_HIDE);
    let title = wide("拖曳選取區域，按 Esc 取消");
    let overlay = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_LAYERED,
        class.as_ptr(),
        title.as_ptr(),
        WS_POPUP,
        x,
        y,
        width,
        height,
        app.hwnd,
        ptr::null_mut(),
        instance,
        ptr::null(),
    );
    if !overlay.is_null() {
        SetLayeredWindowAttributes(overlay, 0, 115, LWA_ALPHA);
        ShowWindow(overlay, SW_SHOW);
        SetFocus(overlay);
        SetCapture(overlay);
        let mut msg: MSG = std::mem::zeroed();
        while IsWindow(overlay) != 0 && GetMessageW(&mut msg, overlay, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    ReleaseCapture();
    SELECT_STATE = ptr::null_mut();
    ShowWindow(app.hwnd, SW_SHOW);
    if let Some(region) = selection.result {
        let base = if mana { ID_MP_BASE } else { ID_HP_BASE };
        for (index, value) in region.iter().enumerate() {
            set_text(app.controls[&(base + 4 + index as i32)], &value.to_string());
        }
        let _ = save_from_controls(app, false, false);
    }
}

unsafe fn choose_json_path(hwnd: HWND, save: bool) -> Option<std::path::PathBuf> {
    let _guard = ReentrantGuard::new();
    let filter = wide("JSON 檔案 (*.json)\0*.json\0所有檔案 (*.*)\0*.*\0\0");
    let title = wide(if save { "匯出設定" } else { "匯入設定" });
    let extension = wide("json");
    let mut filename = vec![0u16; 32768];
    let mut dialog: OPENFILENAMEW = std::mem::zeroed();
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = hwnd;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrFile = filename.as_mut_ptr();
    dialog.nMaxFile = filename.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrDefExt = extension.as_ptr();
    dialog.Flags = OFN_EXPLORER
        | OFN_NOCHANGEDIR
        | OFN_PATHMUSTEXIST
        | if save {
            OFN_OVERWRITEPROMPT
        } else {
            OFN_FILEMUSTEXIST
        };
    let accepted = if save {
        GetSaveFileNameW(&mut dialog)
    } else {
        GetOpenFileNameW(&mut dialog)
    };
    if accepted == 0 {
        return None;
    }
    let end = filename
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filename.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(
        &filename[..end],
    )))
}

unsafe fn export_settings(app: &mut AppState) {
    if !save_from_controls(app, true, false) {
        return;
    }
    let Some(path) = choose_json_path(app.hwnd, true) else {
        return;
    };
    match config::save(&path, &app.settings) {
        Ok(()) => set_text(app.controls[&ID_HP_STATUS], "設定已匯出。"),
        Err(error) => set_error(app, &format!("設定匯出失敗：{error}")),
    }
}

unsafe fn import_settings(app: &mut AppState) {
    if !profile_changes_allowed(app) {
        return;
    }
    let Some(path) = choose_json_path(app.hwnd, false) else {
        return;
    };
    let imported = match config::load(&path) {
        Ok(settings) => settings,
        Err(error) => {
            set_error(app, &format!("設定匯入失敗：{error}"));
            return;
        }
    };
    match config::save(&app.path, &imported) {
        Ok(()) => {
            app.settings = imported;
            app.config_valid = true;
            app.loading_controls = true;
            refresh_profile_tabs(app, app.settings.active_profile);
            load_controls(app);
            app.loading_controls = false;
            set_text(app.controls[&ID_HP_STATUS], "設定已匯入並儲存。");
        }
        Err(error) => set_error(app, &format!("設定匯入後無法寫入本機設定檔：{error}")),
    }
}

unsafe extern "system" fn select_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    let state = SELECT_STATE.as_mut();
    match msg {
        WM_LBUTTONDOWN => {
            if let Some(state) = state {
                let mut p: POINT = std::mem::zeroed();
                GetCursorPos(&mut p);
                state.start = [p.x, p.y];
                state.current = state.start;
                SetCapture(hwnd);
            }
            0
        }
        WM_MOUSEMOVE => {
            if let Some(state) = state {
                if GetCapture() == hwnd {
                    let mut p: POINT = std::mem::zeroed();
                    GetCursorPos(&mut p);
                    state.current = [p.x, p.y];
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            if let Some(state) = state {
                let mut p: POINT = std::mem::zeroed();
                GetCursorPos(&mut p);
                state.current = [p.x, p.y];
                let left = state.start[0].min(p.x);
                let top = state.start[1].min(p.y);
                let right = state.start[0].max(p.x);
                let bottom = state.start[1].max(p.y);
                if right - left >= 10 && bottom - top >= 10 {
                    state.result = Some([left, top, right, bottom]);
                }
            }
            ReleaseCapture();
            DestroyWindow(hwnd);
            0
        }
        WM_KEYDOWN if _wparam as u32 == VK_ESCAPE as u32 => {
            DestroyWindow(hwnd);
            0
        }
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            if let Some(state) = state {
                let pen = CreatePen(PS_SOLID, 2, 0x000000ff);
                let old_pen = SelectObject(dc, pen as _);
                let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH as i32));
                let left = state.start[0].min(state.current[0]);
                let top = state.start[1].min(state.current[1]);
                let right = state.start[0].max(state.current[0]);
                let bottom = state.start[1].max(state.current[1]);
                Rectangle(
                    dc,
                    left - state.origin[0],
                    top - state.origin[1],
                    right - state.origin[0],
                    bottom - state.origin[1],
                );
                SelectObject(dc, old_brush);
                SelectObject(dc, old_pen);
                DeleteObject(pen as _);
            }
            SetTextColor(dc, 0x00ffffff);
            SetBkMode(dc, TRANSPARENT as i32);
            let mut hint = RECT {
                left: 20,
                top: 18,
                right: 520,
                bottom: 50,
            };
            let text = wide("拖曳選取區域，按 Esc 取消");
            DrawTextW(
                dc,
                text.as_ptr(),
                -1,
                &mut hint,
                DT_LEFT | DT_TOP | DT_SINGLELINE,
            );
            EndPaint(hwnd, &paint);
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, _wparam, _lparam),
    }
}

unsafe fn control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: i32,
) -> HWND {
    let class = wide(class);
    let text = wide(text);
    CreateWindowExW(
        0,
        class.as_ptr(),
        text.as_ptr(),
        style,
        x,
        y,
        w,
        h,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(ptr::null()),
        ptr::null(),
    )
}
unsafe fn label(parent: HWND, text: &str, x: i32, y: i32) {
    control(parent, "STATIC", text, WS_CHILD_VISIBLE, x, y, 400, 22, 0);
}
unsafe fn group(parent: HWND, text: &str, x: i32, y: i32, w: i32, h: i32) {
    control(
        parent,
        "BUTTON",
        text,
        WS_CHILD_VISIBLE | BS_GROUPBOX as u32,
        x,
        y,
        w,
        h,
        0,
    );
}
unsafe fn get_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
    String::from_utf16_lossy(&buf[..copied])
}
unsafe fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    SetWindowTextW(hwnd, text.as_ptr());
}
unsafe fn set_error(app: &AppState, text: &str) {
    set_text(app.controls[&ID_HP_STATUS], &format!("設定錯誤：{text}"));
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
