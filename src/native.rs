use crate::{
    config::{self, Settings},
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
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
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
            680,
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
        create_controls(&mut app);
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
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
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
            260,
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
            420,
            14,
            250,
            26,
            ID_AUTO,
        ),
    );
    let auto = if app.settings.ocr.auto_region {
        BST_CHECKED
    } else {
        BST_UNCHECKED
    };
    SendMessageW(app.controls[&ID_AUTO], BM_SETCHECK, auto as usize, 0);
    group(app.hwnd, "HP 補血", 20, 55, 410, 425);
    group(app.hwnd, "MP 補魔", 450, 55, 410, 425);
    for (base, x, mana) in [(ID_HP_BASE, 40, false), (ID_MP_BASE, 470, true)] {
        label(app.hwnd, "低於門檻時補水 (%)", x, 88);
        app.controls.insert(
            base,
            control(
                app.hwnd,
                "EDIT",
                "",
                WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
                x + 170,
                84,
                90,
                24,
                base,
            ),
        );
        label(app.hwnd, "補水按鍵", x, 123);
        app.controls.insert(
            base + 2,
            control(
                app.hwnd,
                "BUTTON",
                "",
                WS_CHILD_VISIBLE | WS_TABSTOP,
                x + 170,
                119,
                120,
                24,
                base + 2,
            ),
        );
        label(app.hwnd, "手動區域 [左, 上, 右, 下]", x, 158);
        for j in 0..4 {
            app.controls.insert(
                base + 4 + j,
                control(
                    app.hwnd,
                    "EDIT",
                    "",
                    WS_CHILD_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32,
                    x + 4 + (j as i32) * 78,
                    185,
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
                217,
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
            330,
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
            390,
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
            330,
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
            390,
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
            505,
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
            505,
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
            505,
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
            505,
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
            505,
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
            505,
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
        560,
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
    Some(match vk {
        0x10 => "Shift", 0x11 => "Ctrl", 0x12 => "Alt",
        0x2e => "Delete", 0x23 => "End", 0x24 => "Home", 0x2d => "Insert",
        0x21 => "PageUp", 0x22 => "PageDown", 0x20 => "Space", 0x0d => "Enter",
        0x09 => "Tab", 0x1b => "Esc", 0x26 => "Up", 0x28 => "Down",
        0x25 => "Left", 0x27 => "Right", 0x08 => "Backspace",
        0xbd => "-", 0xbb => "=", 0xdb => "[", 0xdd => "]", 0xba => ";",
        0xde => "'", 0xbc => ",", 0xbe => ".", 0xbf => "/",
        _ => return None,
    }.to_string())
}

unsafe fn load_controls(app: &AppState) {
    let _guard = ReentrantGuard::new();
    let s = &app.settings.ocr;
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
        (ID_PREFIX, s.pref_var.clone()),
        (ID_HP_BASE, s.th_var.to_string()),
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
}

unsafe fn save_from_controls(
    app: &mut AppState,
    show: bool,
    allow_invalid_replacement: bool,
) -> bool {
    let mut s = app.settings.ocr.clone();
    s.pref_var = get_text(app.controls[&ID_PREFIX]);
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
    app.settings.ocr = s;
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
    if let Err(error) = app.settings.ocr.validate(mana) {
        set_error(app, &error);
        return;
    }
    app.workers[index] = Some(Worker::start(
        mana,
        app.settings.ocr.clone(),
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
        if let Err(error) = app.settings.ocr.validate(mana) {
            set_error(app, &error);
            return;
        }
    }
    app.workers[0] = Some(Worker::start(
        false,
        app.settings.ocr.clone(),
        app.tx.clone(),
    ));
    app.workers[1] = Some(Worker::start(
        true,
        app.settings.ocr.clone(),
        app.tx.clone(),
    ));
    set_text(app.controls[&ID_HP_STATUS], "啟動中…");
    set_text(app.controls[&ID_MP_STATUS], "啟動中…");
    update_worker_buttons(app);
}

unsafe fn update_worker_buttons(app: &AppState) {
    let hp_running = app.workers[0].is_some();
    let mp_running = app.workers[1].is_some();
    set_text(
        app.controls[&ID_START_HP],
        if hp_running { "F6 停止 HP（執行中）" } else { "F6 啟動 HP" },
    );
    set_text(
        app.controls[&ID_START_MP],
        if mp_running { "F7 停止 MP（執行中）" } else { "F7 啟動 MP" },
    );
    set_text(
        app.controls[&ID_START_ALL],
        if hp_running || mp_running { "F5 停止全部" } else { "F5 啟動全部" },
    );
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
