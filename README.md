# msw-auto-heal

MapleStory Worlds HP／MP 自動補水工具（Rust）。

Windows 原生桌面版，提供 HP／MP OCR 監控與補水按鍵輸入。不包含技能循環或自動施放技能。

## 建置與執行

需要 Rust MSVC 工具鏈，以及可由 `PATH` 找到的 Tesseract OCR（英文 `eng` 語言資料）。

```powershell
cargo run --release --offline
```

執行檔旁的 `usersetting.json` 會保存全域遊戲視窗標題前綴和多個角色設定檔。可用頁籤切換設定檔；在「名稱」欄輸入名稱後按「更名」，用「新增」建立目前設定的副本，或用「刪除」移除目前設定檔。監控執行中會鎖定切換與管理功能。舊版單一 `ocr` 設定會自動轉成「預設」設定檔；舊版 `skills` 欄位會忽略。若設定檔格式錯誤，程式使用預設值並保留原檔，成功儲存時以暫存檔取代設定檔。

介面提供自動保存及 JSON 匯入／匯出。匯入的設定會套用到目前程式，並更新執行檔旁的設定檔。

要沿用 Python 版設定，請將 `D:\project\artale_auto-master\usersetting.json` 複製到 Rust 執行檔旁（使用 `cargo run` 時為 `target\release`），再啟動程式。

## 快捷鍵

- `F5`：啟動或停止 HP 與 MP 監控
- `F6`：啟動或停止 HP 監控
- `F7`：啟動或停止 MP 監控

第一次使用請確認遊戲視窗標題前綴、HP／MP 補水門檻及按鍵。可勾選自動定位，或拖曳選取手動擷取區域。監控啟動後才會辨識畫面與送出補水按鍵。

HP 設定可輸入角色 HP 上限與怪物最大傷害；兩者大於 0 時，程式會以最大傷害除以 HP 上限計算建議補水門檻，並自動套用到 HP 門檻欄位。若最大傷害達到 HP 上限，門檻會設為 100%，但仍可能被單次攻擊擊倒。

## 開發檢查

```powershell
cargo fmt --check
cargo test --offline
cargo build --release --offline
```

## 已知限制

- 目前針對 Windows，使用 Win32 原生控制項、GDI 螢幕擷取與全域快捷鍵。
- OCR 透過 Tesseract 命令列程式執行；若無法從 `PATH` 啟動，請將 Tesseract 安裝目錄加入 `PATH`。
- 區域擷取使用螢幕座標。遊戲最小化、受保護畫面或桌面擷取受限時，擷取可能失敗。
- 此版本已完成編譯與離線邏輯測試；實際遊戲視窗中的 OCR 定位、畫面擷取和按鍵輸入仍需在使用者桌面確認。啟用監控前請先檢查畫面、OCR 結果和鍵位設定。
