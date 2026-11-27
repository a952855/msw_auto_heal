use crate::{
    config::OcrSettings,
    ocr::{self, Decision, HealDecision},
};
use crossbeam_channel::Sender;
use std::os::windows::process::CommandExt;
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::*,
    UI::{
        Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP},
        WindowsAndMessaging::*,
    },
};

#[derive(Clone, Copy)]
pub enum Metric {
    Hp,
    Mp,
}
impl Metric {
    fn label(self) -> &'static str {
        match self {
            Self::Hp => "HP",
            Self::Mp => "MP",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MonitorEvent {
    pub mana: bool,
    pub text: String,
    pub scan: Option<String>,
}

pub struct Worker {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start(
        mana: bool,
        settings: OcrSettings,
        window_prefix: String,
        events: Sender<MonitorEvent>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let join =
            thread::spawn(move || monitor_loop(mana, settings, window_prefix, events, thread_stop));
        Self {
            stop,
            join: Some(join),
        }
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn monitor_loop(
    mana: bool,
    settings: OcrSettings,
    window_prefix: String,
    events: Sender<MonitorEvent>,
    stop: Arc<AtomicBool>,
) {
    let metric = if mana { Metric::Mp } else { Metric::Hp };
    let (threshold, key, manual) = if mana {
        (
            settings.mana_th_var,
            settings.mana_key_var.clone(),
            settings.mana_region,
        )
    } else {
        (settings.th_var, settings.key_var.clone(), settings.region)
    };
    let mut decision = HealDecision::default();
    let mut auto_cache: Option<(RECT, [i32; 4])> = None;
    while !stop.load(Ordering::Acquire) {
        let Some((hwnd, bounds, title)) = find_game_window(&window_prefix) else {
            let _ = events.send(MonitorEvent {
                mana,
                text: "找不到符合標題前綴的遊戲視窗".into(),
                scan: None,
            });
            sleep_interruptible(&stop, Duration::from_millis(500));
            continue;
        };
        let region = if settings.auto_region {
            if let Some((cached_bounds, region)) = auto_cache {
                if same_rect(cached_bounds, bounds) {
                    region
                } else {
                    match locate_region(hwnd, bounds, metric) {
                        Ok(region) => {
                            auto_cache = Some((bounds, region));
                            region
                        }
                        Err(error) => {
                            let _ = events.send(MonitorEvent {
                                mana,
                                text: format!("自動定位失敗：{error}"),
                                scan: None,
                            });
                            sleep_interruptible(&stop, Duration::from_millis(500));
                            continue;
                        }
                    }
                }
            } else {
                match locate_region(hwnd, bounds, metric) {
                    Ok(region) => {
                        auto_cache = Some((bounds, region));
                        region
                    }
                    Err(error) => {
                        let _ = events.send(MonitorEvent {
                            mana,
                            text: format!("自動定位失敗：{error}"),
                            scan: None,
                        });
                        sleep_interruptible(&stop, Duration::from_millis(500));
                        continue;
                    }
                }
            }
        } else {
            manual
        };
        let bmp = match capture_bmp(region) {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = events.send(MonitorEvent {
                    mana,
                    text: format!("螢幕擷取失敗：{error}"),
                    scan: None,
                });
                sleep_interruptible(&stop, Duration::from_millis(500));
                continue;
            }
        };
        let mut raw = String::new();
        let mut readings = Vec::new();
        let mut needs_consensus = false;
        for (threshold, psm, scale) in [
            (None, 7, 3),
            (Some(150), 7, 4),
            (Some(125), 7, 4),
            (Some(175), 7, 4),
            (None, 6, 4),
        ] {
            let candidate = process_bmp(&bmp, threshold, scale);
            match tesseract(&candidate, psm, false) {
                Ok(text) => {
                    raw = text.trim().to_owned();
                    if let Some(reading) = ocr::parse_reading(&raw) {
                        readings.push((reading, raw.clone()));
                        let votes = readings.iter().filter(|(seen, _)| *seen == reading).count();
                        // Retry only when the maximum looks like it gained a
                        // leading OCR digit, rather than for every low HP value.
                        let current = reading.current.to_string();
                        let maximum = reading.maximum.to_string();
                        let suspicious = reading.maximum >= reading.current.saturating_mul(5)
                            && maximum.len() == current.len() + 1
                            && maximum.ends_with(&current);
                        needs_consensus |= suspicious;
                        if (!needs_consensus && !suspicious) || votes >= 3 {
                            break;
                        }
                    }
                }
                Err(error) => {
                    raw = error;
                    break;
                }
            }
        }
        let selected = readings
            .iter()
            .max_by_key(|(reading, _)| readings.iter().filter(|(seen, _)| seen == reading).count());
        let reading = selected.and_then(|(reading, text)| {
            let votes = readings.iter().filter(|(seen, _)| seen == reading).count();
            if needs_consensus && votes < 3 {
                raw = format!(
                    "辨識結果不一致：{}",
                    readings
                        .iter()
                        .map(|(value, _)| format!("{}/{}", value.current, value.maximum))
                        .collect::<Vec<_>>()
                        .join("、")
                );
                None
            } else {
                raw = text.clone();
                Some(*reading)
            }
        });
        let Some(reading) = reading else {
            let _ = events.send(MonitorEvent {
                mana,
                text: format!("{title} | 無法解析數值"),
                scan: Some(raw),
            });
            sleep_interruptible(&stop, Duration::from_millis(80));
            continue;
        };
        let now = Instant::now();
        let result = decision.evaluate(reading, threshold, now);
        let percent = reading.current as f64 / reading.maximum as f64 * 100.0;
        let text = match result {
            Decision::Confirming => format!("確認數值中：{}/{}", reading.current, reading.maximum),
            Decision::AwaitingPotion => format!(
                "{}/{} | {:.1}% | 等待補水數值更新",
                reading.current, reading.maximum, percent
            ),
            Decision::SendKey => match send_key(&key) {
                Ok(()) => format!(
                    "{}/{} | {:.1}% | 已送出補水鍵，等待數值更新",
                    reading.current, reading.maximum, percent
                ),
                Err(error) => format!("按鍵輸入失敗：{error}"),
            },
            Decision::Idle => format!("{}/{} | {:.1}%", reading.current, reading.maximum, percent),
        };
        let _ = events.send(MonitorEvent {
            mana,
            text,
            scan: Some(format!("讀值：{}/{}", reading.current, reading.maximum)),
        });
        sleep_interruptible(&stop, Duration::from_millis(40));
    }
}

fn same_rect(a: RECT, b: RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

fn sleep_interruptible(stop: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
}

fn find_game_window(prefix: &str) -> Option<(HWND, RECT, String)> {
    struct Search {
        prefix: String,
        found: Option<(HWND, RECT, String)>,
    }
    unsafe extern "system" fn callback(hwnd: HWND, param: isize) -> i32 {
        let search = &mut *(param as *mut Search);
        if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
            return 1;
        }
        let mut title = [0u16; 512];
        let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if len <= 0 {
            return 1;
        }
        let text = String::from_utf16_lossy(&title[..len as usize]);
        if !text.starts_with(&search.prefix) {
            return 1;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) != 0 && rect.right > rect.left && rect.bottom > rect.top {
            search.found = Some((hwnd, rect, text));
            return 0;
        }
        1
    }
    let mut search = Search {
        prefix: prefix.to_owned(),
        found: None,
    };
    unsafe {
        EnumWindows(Some(callback), &mut search as *mut _ as isize);
    }
    search.found
}

fn locate_region(hwnd: HWND, bounds: RECT, metric: Metric) -> Result<[i32; 4], String> {
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let top = bounds.top + (height as f32 * 0.68) as i32;
    let area = [bounds.left, top, bounds.right, bounds.bottom];
    let bmp = capture_bmp(area)?;
    let data = tesseract(&bmp, 11, true)?;
    let wanted = metric.label();
    let mut labels: Vec<(String, i32, i32, i32, i32)> = Vec::new();
    for (index, line) in data.lines().enumerate().skip(1) {
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 12 {
            continue;
        }
        let word = columns[11].trim().to_ascii_uppercase();
        if !["HP", "MP", "EXP"].contains(&word.as_str()) {
            continue;
        }
        if let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
            columns[6].parse(),
            columns[7].parse(),
            columns[8].parse(),
            columns[9].parse(),
        ) {
            let _ = index;
            labels.push((word, x, y, w, h));
        }
    }
    let (_, x, y, w, h) = labels
        .iter()
        .filter(|item| item.0 == wanted)
        .min_by_key(|item| item.2)
        .ok_or_else(|| format!("找不到 {wanted} 標籤，請確認遊戲狀態列可見或使用手動區域"))?;
    let next_x = labels
        .iter()
        .filter(|item| (item.0 == "MP" || item.0 == "EXP") && item.1 > *x)
        .map(|item| item.1)
        .min()
        .unwrap_or(width - 8);
    let rel_left = (*x - 4).max(0);
    let rel_top = (y - 7).max(0);
    let rel_right = (next_x - 2).min(width);
    let rel_bottom = (y + h + 9).min(height);
    if rel_right - rel_left < 30 || rel_bottom <= rel_top {
        return Err("標籤區域太小".into());
    }
    let _ = (hwnd, w);
    Ok([
        bounds.left + rel_left,
        bounds.top + top - bounds.top + rel_top,
        bounds.left + rel_right,
        bounds.top + top - bounds.top + rel_bottom,
    ])
}

fn tesseract(bmp: &[u8], psm: u8, tsv: bool) -> Result<String, String> {
    let whitelist = if tsv { "HPMP" } else { "0123456789/[]HP" };
    let psm_arg = psm.to_string();
    let mut command = Command::new("tesseract");
    // Tesseract is a console program; keep its child process hidden in the GUI app.
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command.args([
        "stdin",
        "stdout",
        "--oem",
        "3",
        "--psm",
        &psm_arg,
        "-c",
        &format!("tessedit_char_whitelist={whitelist}"),
    ]);
    if tsv {
        command.arg("tsv");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("無法啟動 Tesseract：{e}，請安裝並加入 PATH"))?;
    child
        .stdin
        .take()
        .ok_or("無法連接 Tesseract 標準輸入")?
        .write_all(bmp)
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Tesseract 辨識逾時".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn process_bmp(source: &[u8], threshold: Option<u8>, scale: usize) -> Vec<u8> {
    if source.len() < 54 {
        return source.to_vec();
    }
    let width =
        i32::from_le_bytes(source[18..22].try_into().unwrap_or([0; 4])).unsigned_abs() as usize;
    let height =
        i32::from_le_bytes(source[22..26].try_into().unwrap_or([0; 4])).unsigned_abs() as usize;
    if width == 0
        || height == 0
        || width.saturating_mul(height) > 1_000_000
        || source.len() < 54 + width.saturating_mul(height).saturating_mul(4)
    {
        return source.to_vec();
    }
    let out_width = width * scale;
    let out_height = height * scale;
    let data_len = out_width * out_height * 4;
    let mut result = source[..54].to_vec();
    result[2..6].copy_from_slice(&((54 + data_len) as u32).to_le_bytes());
    result[18..22].copy_from_slice(&(out_width as i32).to_le_bytes());
    result[22..26].copy_from_slice(&(-(out_height as i32)).to_le_bytes());
    result[34..38].copy_from_slice(&(data_len as u32).to_le_bytes());
    let input = &source[54..54 + width * height * 4];
    let mut gray = Vec::with_capacity(width * height);
    for pixel in input.chunks_exact(4) {
        gray.push(0.114 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.299 * pixel[2] as f32);
    }
    let mean = gray.iter().sum::<f32>() / gray.len() as f32;
    for value in &mut gray {
        *value = (mean + (*value - mean) * 1.8).clamp(0.0, 255.0);
    }
    let mut sharpened = vec![0.0; gray.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let sx = (x as isize + dx).clamp(0, width as isize - 1) as usize;
                    let sy = (y as isize + dy).clamp(0, height as isize - 1) as usize;
                    sum += gray[sy * width + sx];
                }
            }
            let index = y * width + x;
            let blur = sum / 9.0;
            sharpened[index] = (blur + (gray[index] - blur) * 1.5).clamp(0.0, 255.0);
        }
    }
    let resized = resize_lanczos3(&sharpened, width, height, scale);
    result.resize(54 + data_len, 0);
    for (index, gray) in resized.into_iter().enumerate() {
        let value = if let Some(threshold) = threshold {
            if gray >= threshold as f32 {
                255
            } else {
                0
            }
        } else {
            gray.round() as u8
        };
        let out = 54 + index * 4;
        result[out] = value;
        result[out + 1] = value;
        result[out + 2] = value;
        result[out + 3] = 255;
    }
    result
}

fn resize_lanczos3(source: &[f32], width: usize, height: usize, scale: usize) -> Vec<f32> {
    let out_width = width * scale;
    let out_height = height * scale;
    let mut horizontal = vec![0.0; out_width * height];
    for y in 0..height {
        for x in 0..out_width {
            let center = (x as f32 + 0.5) / scale as f32 - 0.5;
            let first = (center - 3.0).floor() as isize;
            let last = (center + 3.0).ceil() as isize;
            let mut value = 0.0;
            let mut weight_sum = 0.0;
            for sx in first..=last {
                let weight = lanczos3(center - sx as f32);
                let sample = sx.clamp(0, width as isize - 1) as usize;
                value += source[y * width + sample] * weight;
                weight_sum += weight;
            }
            horizontal[y * out_width + x] = if weight_sum == 0.0 {
                0.0
            } else {
                value / weight_sum
            };
        }
    }
    let mut resized = vec![0.0; out_width * out_height];
    for y in 0..out_height {
        for x in 0..out_width {
            let center = (y as f32 + 0.5) / scale as f32 - 0.5;
            let first = (center - 3.0).floor() as isize;
            let last = (center + 3.0).ceil() as isize;
            let mut value = 0.0;
            let mut weight_sum = 0.0;
            for sy in first..=last {
                let weight = lanczos3(center - sy as f32);
                let sample = sy.clamp(0, height as isize - 1) as usize;
                value += horizontal[sample * out_width + x] * weight;
                weight_sum += weight;
            }
            resized[y * out_width + x] = if weight_sum == 0.0 {
                0.0
            } else {
                value / weight_sum
            };
        }
    }
    resized
}

fn lanczos3(x: f32) -> f32 {
    if x == 0.0 {
        1.0
    } else if x.abs() >= 3.0 {
        0.0
    } else {
        let pix = std::f32::consts::PI * x;
        (pix.sin() / pix) * ((pix / 3.0).sin() / (pix / 3.0))
    }
}

fn send_key(key: &str) -> Result<(), String> {
    let normalized = key.trim().to_ascii_lowercase();
    let vk = match normalized.as_str() {
        "shift" | "shift_l" | "shift_r" => 0x10,
        "ctrl" | "control" | "control_l" | "control_r" => 0x11,
        "alt" | "alt_l" | "alt_r" => 0x12,
        "delete" => 0x2e,
        "end" => 0x23,
        "home" => 0x24,
        "insert" => 0x2d,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "space" => 0x20,
        "enter" | "return" => 0x0d,
        "tab" => 0x09,
        "esc" | "escape" => 0x1b,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "backspace" => 0x08,
        "capslock" | "caps_lock" => 0x14,
        "numlock" | "num_lock" => 0x90,
        "scrolllock" | "scroll_lock" => 0x91,
        "win" | "windows" => 0x5b,
        value
            if value
                .strip_prefix('f')
                .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))) =>
        {
            0x70 + value[1..].parse::<u8>().unwrap_or(1) as u16 - 1
        }
        "-" => 0xbd,
        "=" => 0xbb,
        "[" => 0xdb,
        "]" => 0xdd,
        ";" => 0xba,
        "'" => 0xde,
        "," => 0xbc,
        "." => 0xbe,
        "/" => 0xbf,
        value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
            value.as_bytes()[0].to_ascii_uppercase() as u16
        }
        _ => return Err(format!("不支援的按鍵名稱：{key}")),
    };
    unsafe {
        keybd_event(vk as u8, 0, 0, 0);
        thread::sleep(Duration::from_millis(80));
        keybd_event(vk as u8, 0, KEYEVENTF_KEYUP, 0);
    }
    Ok(())
}

fn capture_bmp(rect: [i32; 4]) -> Result<Vec<u8>, String> {
    let (left, top, right, bottom) = (rect[0], rect[1], rect[2], rect[3]);
    let (width, height) = (right - left, bottom - top);
    if width <= 0
        || height <= 0
        || width > 8192
        || height > 8192
        || width as i64 * height as i64 > 16_000_000
    {
        return Err("擷取區域大小無效".into());
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return Err("無法取得桌面裝置內容".into());
        }
        let memory = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        info.bmiHeader.biSizeImage = (width * height * 4) as u32;
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            screen,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if bitmap.is_null() || memory.is_null() || bits.is_null() {
            if !memory.is_null() {
                DeleteDC(memory);
            }
            ReleaseDC(std::ptr::null_mut(), screen);
            return Err("無法建立桌面擷取緩衝區".into());
        }
        let old = SelectObject(memory, bitmap as _);
        let copied = BitBlt(memory, 0, 0, width, height, screen, left, top, SRCCOPY);
        let size = (width * height * 4) as usize;
        let mut file = Vec::with_capacity(54 + size);
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&((54 + size) as u32).to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&(40u32).to_le_bytes());
        file.extend_from_slice(&width.to_le_bytes());
        file.extend_from_slice(&(-height).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&(size as u32).to_le_bytes());
        file.extend_from_slice(&[0; 16]);
        if copied != 0 {
            file.extend_from_slice(std::slice::from_raw_parts(bits as *const u8, size));
        }
        SelectObject(memory, old);
        DeleteObject(bitmap as _);
        DeleteDC(memory);
        ReleaseDC(std::ptr::null_mut(), screen);
        if copied == 0 {
            Err("BitBlt 讀取畫面失敗".into())
        } else {
            Ok(file)
        }
    }
}
