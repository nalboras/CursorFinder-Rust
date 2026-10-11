//! Win32 implementation: tray icon, shake detection timers, layered
//! transparent overlay window that draws the enlarged cursor, settings
//! dialog, autostart registry entry.
//!
//! The original was a full-screen transparent Qt widget with a label on it.
//! Here the overlay is a small layered window that is resized/moved to wrap
//! exactly the enlarged image - simpler, multi-monitor friendly, and no
//! taskbar hacks needed (WS_EX_TOOLWINDOW never gets a taskbar button).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::GdiPlus::{
    FillModeAlternate, GdipAddPathPolygon, GdipCreateFromHDC,
    GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteGraphics,
    GdipDeletePath, GdipDeletePen, GdipDisposeImage, GdipDrawImageRectI, GdipDrawPath,
    GdipFillPath, GdipGetImageHeight, GdipGetImageWidth, GdipLoadImageFromFile,
    GdipSetInterpolationMode, GdipSetPenLineJoin, GdipSetSmoothingMode, GdiplusShutdown,
    GdiplusStartup, GdiplusStartupInput, GpBrush, GpGraphics, GpImage, GpPath, GpPen,
    GpSolidFill, InterpolationModeHighQualityBicubic, LineJoinRound, PointF,
    SmoothingModeAntiAlias, Status, UnitPixel,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETMARGINS, MEASUREITEMSTRUCT, ODT_BUTTON, ODT_MENU, ODS_FOCUS,
    ODS_SELECTED,
};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::arrow::{ARROW_DIMS, ARROW_H, ARROW_POINTS, ARROW_STROKE, ARROW_W};
use crate::detect::{ShakeDetector, Signal};
use crate::geometry::Point as GPoint;
use crate::settings::{Settings, DIST_PX_RANGE, GAP_MS_RANGE};
use crate::visibility::ShakeVisibility;

/// GDI+ "Ok" status (the crate exports it as a const named `Ok`, which would
/// collide with `Result::Ok`).
const GP_OK: Status = Status(0);

// ---------------------------------------------------------------- constants

const IDT_DETECT: usize = 1;
const IDT_FOLLOW: usize = 2;
const IDT_MENUFRAME: usize = 3;

const DETECT_INTERVAL_MS: u32 = 20;
const FOLLOW_INTERVAL_MS: u32 = 10;

const MIN_CURSOR_WIDTH: f64 = 30.0;
const MAX_CURSOR_WIDTH: f64 = 110.0;

const IDM_FULLSCREEN: u16 = 1001;
const IDM_AUTOSTART: u16 = 1002;
const IDM_SETTINGS: u16 = 1003;
const IDM_QUIT: u16 = 1004;

const ID_EDIT_GAP: i32 = 2001;
const ID_EDIT_DIST: i32 = 2002;
const ID_BTN_APPLY: i32 = 2003;

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;

const OVERLAY_CLASS: &str = "CursorFinderOverlay";
const MSG_CLASS: &str = "CursorFinderMsg";
const DIALOG_CLASS: &str = "CursorFinderSettings";

const APP_NAME: &str = "CursorFinder";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

// ---------------------------------------------------------------- helpers

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

fn is_cursor_hidden() -> bool {
    let mut info = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetCursorInfo(&mut info).is_err() {
            return false;
        }
    }
    // A blank cursor still counts as CURSOR_SHOWING; only ShowCursor(false)
    // clears the flag - same semantics the original relied on.
    (info.flags.0 & CURSOR_SHOWING.0) == 0
}

/// True when the foreground window covers the whole monitor under the cursor
/// (two-step check, ported from the original).
fn is_fore_fullscreen() -> bool {
    unsafe {
        let hmon = MonitorFromPoint(cursor_pos(), MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return false;
        }
        let screen = mi.rcMonitor;

        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return false;
        }
        // Top-level window owning the bottom-left pixel (taskbar unless a
        // full screen window covers it).
        let hit = WindowFromPoint(POINT {
            x: screen.left,
            y: screen.bottom - 1,
        });
        let root = GetAncestor(hit, GA_ROOT);
        if hwnd != root {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        (r.right - r.left) >= (screen.right - screen.left)
            && (r.bottom - r.top) >= (screen.bottom - screen.top)
    }
}

// ---------------------------------------------------------------- app state

enum CursorImage {
    /// Crisp vector arrow drawn by GDI+ at the animated size (the default).
    Arrow,
    GdiPlus {
        image: *mut GpImage,
        w: u32,
        h: u32,
    },
    Icon {
        handle: HICON,
        is_cursor: bool,
        w: u32,
        h: u32,
    },
}

impl CursorImage {
    fn dims(&self) -> (u32, u32) {
        match self {
            CursorImage::Arrow => ARROW_DIMS,
            CursorImage::GdiPlus { w, h, .. } => (*w, *h),
            CursorImage::Icon { w, h, .. } => (*w, *h),
        }
    }
}

struct App {
    msg_hwnd: HWND,
    overlay_hwnd: HWND,
    settings_hwnd: HWND,
    detector: ShakeDetector,
    settings: Settings,
    settings_path: PathBuf,
    exe_path: PathBuf,
    image: CursorImage,
    gdiplus_token: usize,
    taskbar_created_msg: u32,
    visible: bool,
    anim_t: f64,
    visibility: ShakeVisibility,
    tray_icon: HICON,
    tray_icon_owned: bool,
}

thread_local! {
    static SMALL_CURSOR_HIDDEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LAST_POS: std::cell::Cell<Option<(i32, i32)>> = const { std::cell::Cell::new(None) };
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

// ---------------------------------------------------------------- settings

fn exe_path() -> PathBuf {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

fn load_settings(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(content) => Settings::parse(&content),
        Err(_) => Settings::default(),
    }
}

fn save_settings(settings: &Settings, path: &Path) {
    let _ = std::fs::write(path, settings.serialize());
}

// ---------------------------------------------------------------- autostart

fn open_run_key(access: REG_SAM_FLAGS) -> Option<HKEY> {
    let mut hkey = HKEY::default();
    let sub = wide(RUN_KEY);
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, pcwstr(&sub), None, access, &mut hkey) };
    if status.is_ok() {
        Some(hkey)
    } else {
        None
    }
}

fn is_auto_run(exe: &Path) -> bool {
    let Some(hkey) = open_run_key(KEY_QUERY_VALUE) else {
        return false;
    };
    let name = wide(APP_NAME);
    unsafe {
        let mut size: u32 = 0;
        let status = RegQueryValueExW(hkey, pcwstr(&name), None, None, None, Some(&mut size));
        if status.is_err() || size == 0 {
            let _ = RegCloseKey(hkey);
            return false;
        }
        let mut buf = vec![0u8; size as usize];
        let status = RegQueryValueExW(
            hkey,
            pcwstr(&name),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        );
        let _ = RegCloseKey(hkey);
        if status.is_err() {
            return false;
        }
        let u16s: Vec<u16> = buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let stored = String::from_utf16_lossy(&u16s);
        let stored = stored.trim_matches('"');
        stored.eq_ignore_ascii_case(&exe.to_string_lossy())
    }
}

fn set_auto_run(enable: bool, exe: &Path) {
    let Some(hkey) = open_run_key(KEY_SET_VALUE) else {
        return;
    };
    let name = wide(APP_NAME);
    unsafe {
        if enable {
            // Quoted so paths containing spaces still launch correctly (the
            // original wrote the raw path, which breaks in that case).
            let value = format!("\"{}\"", exe.to_string_lossy());
            let data = wide(&value);
            let bytes: &[u8] =
                std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
            let _ = RegSetValueExW(hkey, pcwstr(&name), None, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(hkey, pcwstr(&name));
        }
        let _ = RegCloseKey(hkey);
    }
}

// ---------------------------------------------------------------- images

fn gp_image_dims(image: *mut GpImage) -> (u32, u32) {
    let (mut w, mut h) = (0u32, 0u32);
    unsafe {
        let _ = GdipGetImageWidth(image, &mut w);
        let _ = GdipGetImageHeight(image, &mut h);
    }
    if w == 0 || h == 0 {
        (32, 32)
    } else {
        (w, h)
    }
}

fn icon_dims(handle: HICON) -> (u32, u32) {
    let mut info = ICONINFO::default();
    unsafe {
        if GetIconInfo(handle, &mut info).is_err() {
            return (32, 32);
        }
        let mut bmp = BITMAP::default();
        let src = if info.hbmColor.is_invalid() {
            info.hbmMask
        } else {
            info.hbmColor
        };
        let mut dims = (32, 32);
        if GetObjectW(
            HGDIOBJ(src.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut BITMAP as *mut core::ffi::c_void),
        ) != 0
        {
            let h = if info.hbmColor.is_invalid() {
                bmp.bmHeight / 2
            } else {
                bmp.bmHeight
            };
            if bmp.bmWidth > 0 && h > 0 {
                dims = (bmp.bmWidth as u32, h as u32);
            }
        }
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        dims
    }
}

fn load_icon_file(path: &Path, is_cursor: bool) -> Option<CursorImage> {
    let wide_path = wide(&path.to_string_lossy());
    let kind = if is_cursor { IMAGE_CURSOR } else { IMAGE_ICON };
    let handle = unsafe {
        LoadImageW(
            None,
            pcwstr(&wide_path),
            kind,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        )
        .ok()?
    };
    if handle.is_invalid() {
        return None;
    }
    let hicon = HICON(handle.0);
    let (w, h) = icon_dims(hicon);
    Some(CursorImage::Icon {
        handle: hicon,
        is_cursor,
        w,
        h,
    })
}

fn load_gp_image_from_file(path: &Path) -> Option<CursorImage> {
    let wide_path = wide(&path.to_string_lossy());
    let mut image: *mut GpImage = ptr::null_mut();
    let status = unsafe { GdipLoadImageFromFile(pcwstr(&wide_path), &mut image) };
    if status != GP_OK || image.is_null() {
        return None;
    }
    let (w, h) = gp_image_dims(image);
    Some(CursorImage::GdiPlus { image, w, h })
}

/// Load the cursor image: a custom `cursor.*` file next to the exe if one
/// exists, otherwise the embedded default PNG.
fn load_cursor_image(exe_dir: &Path) -> CursorImage {
    for ext in ["cur", "ico", "png", "jpg", "jpeg"] {
        let path = exe_dir.join(format!("cursor.{ext}"));
        if !path.is_file() {
            continue;
        }
        if ext == "cur" {
            // GDI+ cannot load .cur; go straight to LoadImage.
            if let Some(img) = load_icon_file(&path, true) {
                return img;
            }
            continue;
        }
        if let Some(img) = load_gp_image_from_file(&path) {
            return img;
        }
        if let Some(img) = load_icon_file(&path, false) {
            return img;
        }
    }

    // Default: the vector arrow, drawn at the animated size so it stays
    // crisp (a scaled bitmap is what looked pixelated before).
    CursorImage::Arrow
}

// ---------------------------------------------------------------- overlay

/// Draw the classic arrow polygon at the current size: white fill, dark
/// outline, anti-aliased. Everything scales with the target size, so both
/// the growing animation and the final enlarged cursor stay sharp.
unsafe fn draw_arrow(graphics: *mut GpGraphics, w: i32, h: i32) {
    let mut path: *mut GpPath = ptr::null_mut();
    if GdipCreatePath(FillModeAlternate, &mut path) != GP_OK || path.is_null() {
        return;
    }
    // Leave room for half the stroke width so the outline never clips.
    let pad = (ARROW_STROKE / 2.0 + 0.3) * (w as f32 / ARROW_W);
    let sx = (w as f32 - 2.0 * pad) / ARROW_W;
    let sy = (h as f32 - 2.0 * pad) / ARROW_H;
    let points: Vec<PointF> = ARROW_POINTS
        .iter()
        .map(|&(x, y)| PointF {
            X: pad + x * sx,
            Y: pad + y * sy,
        })
        .collect();
    if GdipAddPathPolygon(path, points.as_ptr(), points.len() as i32) == GP_OK {
        let mut brush: *mut GpSolidFill = ptr::null_mut();
        if GdipCreateSolidFill(0xFFFFFFFF, &mut brush) == GP_OK && !brush.is_null() {
            let _ = GdipFillPath(graphics, brush as *mut GpBrush, path);
            let _ = GdipDeleteBrush(brush as *mut GpBrush);
        }
        let mut pen: *mut GpPen = ptr::null_mut();
        if GdipCreatePen1(0xFF000000, ARROW_STROKE * sx, UnitPixel, &mut pen) == GP_OK
            && !pen.is_null()
        {
            let _ = GdipSetPenLineJoin(pen, LineJoinRound);
            let _ = GdipDrawPath(graphics, pen, path);
            let _ = GdipDeletePen(pen);
        }
    }
    let _ = GdipDeletePath(path);
}

/// Render the cursor image at animation progress `t` (0..1) centered on the
/// real cursor position, and move/resize the layered window over it.
unsafe fn render_overlay(app: &App) {
    let t = app.anim_t.clamp(0.0, 1.0);
    let (iw, ih) = app.image.dims();
    let w = (MIN_CURSOR_WIDTH + t * (MAX_CURSOR_WIDTH - MIN_CURSOR_WIDTH)).round() as i32;
    let w = w.max(1);
    let h = ((w as i64 * ih as i64) / iw as i64) as i32;
    let h = h.max(1);

    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits: *mut core::ffi::c_void = ptr::null_mut();
    let Ok(hbmp) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        return;
    };
    if bits.is_null() {
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        return;
    }
    let pixel_count = (w * h) as usize;
    ptr::write_bytes(bits as *mut u8, 0, pixel_count * 4);

    let memdc = CreateCompatibleDC(None);
    if memdc.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        return;
    }
    let old = SelectObject(memdc, HGDIOBJ(hbmp.0));

    match app.image {
        CursorImage::Arrow => {
            let mut graphics: *mut GpGraphics = ptr::null_mut();
            if GdipCreateFromHDC(memdc, &mut graphics) == GP_OK && !graphics.is_null() {
                let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
                draw_arrow(graphics, w, h);
                let _ = GdipDeleteGraphics(graphics);
            }
        }
        CursorImage::GdiPlus { image, .. } => {
            let mut graphics: *mut GpGraphics = ptr::null_mut();
            if GdipCreateFromHDC(memdc, &mut graphics) == GP_OK && !graphics.is_null() {
                let _ = GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);
                let _ = GdipDrawImageRectI(graphics, image, 0, 0, w, h);
                let _ = GdipDeleteGraphics(graphics);
            }
        }
        CursorImage::Icon { handle, .. } => {
            let _ = DrawIconEx(memdc, 0, 0, handle, w, h, 0, None, DI_NORMAL);
        }
    }

    // Fade by scaling the premultiplied pixels (cheaper and simpler than a
    // GDI+ color matrix, identical result).
    if t < 1.0 {
        let px = bits as *mut u8;
        for i in 0..pixel_count * 4 {
            let p = px.add(i);
            *p = (*p as f64 * t).round() as u8;
        }
    }

    // Center on the cursor (the original anchored the image's top-left corner
    // at the pointer, which pushed the enlarged cursor down-right instead of
    // growing it in place like macOS does).
    let pos = cursor_pos();
    let dst = POINT {
        x: pos.x - w / 2,
        y: pos.y - h / 2,
    };
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = UpdateLayeredWindow(
        app.overlay_hwnd,
        None,
        Some(&dst),
        Some(&size),
        Some(memdc),
        Some(&src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );

    SelectObject(memdc, old);
    let _ = DeleteDC(memdc);
    let _ = DeleteObject(HGDIOBJ(hbmp.0));
}

/// Re-assert the topmost z-order: another topmost window (maximized or
/// borderless full screen app, taskbar) can end up above the overlay, which
/// hides the enlarged cursor on that monitor.
unsafe fn assert_topmost(hwnd: HWND) {
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
}

/// Replace the system arrow with a fully transparent one while the enlarged
/// overlay is shown, so only one cursor is visible.
fn hide_small_cursor() {
    if SMALL_CURSOR_HIDDEN.with(|c| c.get()) {
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(PCWSTR::null()).map(|h| HINSTANCE(h.0)).unwrap_or_default();
        let and_mask = [0xFFu8; 128];
        let xor_mask = [0u8; 128];
        if let Ok(blank) = CreateCursor(Some(hinst), 0, 0, 32, 32, and_mask.as_ptr().cast(), xor_mask.as_ptr().cast()) {
            if SetSystemCursor(blank, SYSTEM_CURSOR_ID(32512)).is_ok() {
                SMALL_CURSOR_HIDDEN.with(|c| c.set(true));
            } else {
                let _ = DestroyCursor(blank);
            }
        }
    }
}

/// Reload the user's real system cursors from the registry.
fn restore_small_cursor() {
    if SMALL_CURSOR_HIDDEN.with(|c| c.replace(false)) {
        unsafe {
            let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
    }
}

fn show_overlay(app: &mut App) {
    app.visible = true;
    app.anim_t = 0.0;
    unsafe {
        let _ = ShowWindow(app.overlay_hwnd, SW_SHOWNOACTIVATE);
        assert_topmost(app.overlay_hwnd);
        let _ = SetTimer(Some(app.msg_hwnd), IDT_FOLLOW, FOLLOW_INTERVAL_MS, None);
        render_overlay(app);
        hide_small_cursor();
    }
}

fn follow_tick(app: &mut App) {
    let now = Instant::now();
    let lifetime = app.visibility.fraction(now);
    if lifetime == 0.0 {
        unsafe {
            let _ = KillTimer(Some(app.msg_hwnd), IDT_FOLLOW);
            let _ = ShowWindow(app.overlay_hwnd, SW_HIDE);
        }
        restore_small_cursor();
        app.visible = false;
        app.anim_t = 0.0;
        return;
    }
    app.anim_t = lifetime;
    unsafe {
        assert_topmost(app.overlay_hwnd);
        render_overlay(app);
    }
}

fn detect_tick(app: &mut App) {
    if is_cursor_hidden() {
        return;
    }
    let pos = cursor_pos();
    let gpos = GPoint::new(pos.x as f64, pos.y as f64);
    let now = Instant::now();

    // Software KVMs (DeskFlow etc.) warp the pointer when switching screens.
    // A jump this large in one 20 ms tick is not hand movement: drop the
    // enlargement instead of following the warp.
    let prev = LAST_POS.with(|c| c.replace(Some((pos.x, pos.y))));
    if let Some((px, py)) = prev {
        let (dx, dy) = ((pos.x - px) as f64, (pos.y - py) as f64);
        if app.visible && (dx * dx + dy * dy).sqrt() > 1000.0 {
            app.visibility = ShakeVisibility::default();
            return;
        }
    }

    app.visibility.motion(gpos, now);
    match app.detector.sample(gpos, now) {
        Signal::Corner => {
            // Fullscreen suppression happens here, not in the detector, to
            // keep the per-tick cost down - same as the original.
            if app.settings.disable_when_fullscreen && is_fore_fullscreen() {
                return;
            }
            if app.detector.corner(gpos, now) {
                app.visibility.shake(now);
                if !app.visible {
                    show_overlay(app);
                }
            }
        }
        // Visibility tracks motion between confirmed reversals.
        Signal::Slow | Signal::None => {}
    }
}

// ---------------------------------------------------------------- tray

fn tray_icon_handle() -> (HICON, bool) {
    let data: &[u8] = include_bytes!("../assets/ICON.ico");
    unsafe {
        // Match the tray's small-icon metrics (including scaled displays).
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        if let Some(image) = crate::ico::image_for_size(data, size as u32) {
            if let Ok(icon) =
                CreateIconFromResourceEx(image, true, 0x00030000, size, size, LR_DEFAULTCOLOR)
            {
                if !icon.is_invalid() {
                    return (icon, true);
                }
            }
        }
        // Never submit a null handle if custom decoding fails. Stock handles
        // are shared and must not be destroyed.
        (
            LoadIconW(None, IDI_APPLICATION).expect("could not load fallback tray icon"),
            false,
        )
    }
}

fn add_tray_icon(app: &App, show_balloon: bool) {
    unsafe {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            ..Default::default()
        };
        nid.hWnd = app.msg_hwnd;
        nid.uID = TRAY_UID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = app.tray_icon;
        let tip = wide("CursorFinder - shake to enlarge cursor");
        let n = tip.len().min(127);
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);

        if show_balloon {
            nid.uFlags = NIF_INFO;
            let info = wide("Shake your mouse to enlarge the cursor.");
            let title = wide("CursorFinder has started");
            let ni = info.len().min(255);
            nid.szInfo[..ni].copy_from_slice(&info[..ni]);
            let nt = title.len().min(63);
            nid.szInfoTitle[..nt].copy_from_slice(&title[..nt]);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }
}

fn remove_tray_icon(app: &App) {
    unsafe {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            ..Default::default()
        };
        nid.hWnd = app.msg_hwnd;
        nid.uID = TRAY_UID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

thread_local! {
    static MENU_FONT: RefCell<Option<HFONT>> = const { RefCell::new(None) };
}

fn menu_font() -> Option<HFONT> {
    MENU_FONT.with(|f| *f.borrow())
}

fn menu_item_label(id: u32) -> Option<&'static str> {
    match id as u16 {
        IDM_FULLSCREEN => Some("Disable while full screen"),
        IDM_AUTOSTART => Some("Run at startup"),
        IDM_SETTINGS => Some("Settings..."),
        IDM_QUIT => Some("Quit"),
        _ => None,
    }
}

fn menu_item_checked(id: u32) -> bool {
    with_app(|app| match id as u16 {
        IDM_FULLSCREEN => app.settings.disable_when_fullscreen,
        IDM_AUTOSTART => is_auto_run(&app.exe_path),
        _ => false,
    })
    .unwrap_or(false)
}

unsafe fn measure_menu_item(mis: &mut MEASUREITEMSTRUCT) {
    let dpi = GetDpiForSystem();
    let sc = |v: i32| v * dpi as i32 / 96;
    if menu_item_label(mis.itemID).is_none() {
        // Separator.
        mis.itemWidth = sc(160) as u32;
        mis.itemHeight = sc(9) as u32;
        return;
    }
    let label: Vec<u16> = menu_item_label(mis.itemID).unwrap_or("").encode_utf16().collect();
    let mut size = SIZE::default();
    if let Some(font) = menu_font() {
        let hdc = GetDC(None);
        if !hdc.is_invalid() {
            let old = SelectObject(hdc, HGDIOBJ(font.0));
            let _ = GetTextExtentPoint32W(hdc, &label, &mut size);
            SelectObject(hdc, old);
            ReleaseDC(None, hdc);
        }
    }
    // Check gutter + text + right padding.
    mis.itemWidth = (sc(56) + size.cx) as u32;
    mis.itemHeight = sc(28) as u32;
}

unsafe fn draw_menu_item(dis: &DRAWITEMSTRUCT) {
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let dpi = GetDpiForSystem();
    let sc = |v: i32| v * dpi as i32 / 96;

    let bg = CreateSolidBrush(ui_bg());
    FillRect(hdc, &rc, bg);
    let _ = DeleteObject(HGDIOBJ(bg.0));

    if menu_item_label(dis.itemID).is_none() {
        // Separator: 1 px line, vertically centered.
        let pen = CreatePen(PS_SOLID, 1, rgb(68, 68, 68));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
        let y = (rc.top + rc.bottom) / 2;
        let _ = MoveToEx(hdc, rc.left + sc(12), y, None);
        let _ = LineTo(hdc, rc.right - sc(12), y);
        SelectObject(hdc, old_pen);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        return;
    }

    if (dis.itemState.0 & ODS_SELECTED.0) != 0 {
        // Rounded, slightly inset highlight (Windows 11 style).
        let hb = CreateSolidBrush(rgb(62, 62, 62));
        let old_brush = SelectObject(hdc, HGDIOBJ(hb.0));
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = RoundRect(
            hdc,
            rc.left + sc(3),
            rc.top + 1,
            rc.right - sc(3),
            rc.bottom - 1,
            sc(6),
            sc(6),
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(HGDIOBJ(hb.0));
    }

    if let Some(font) = menu_font() {
        if menu_item_checked(dis.itemID) {
            let mut check_rc = RECT {
                left: rc.left,
                top: rc.top,
                right: rc.left + sc(30),
                bottom: rc.bottom,
            };
            draw_text(hdc, font, ui_text(), &mut check_rc, "\u{2713}", true);
        }
        let mut text_rc = RECT {
            left: rc.left + sc(34),
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        };
        draw_text(
            hdc,
            font,
            ui_text(),
            &mut text_rc,
            menu_item_label(dis.itemID).unwrap_or(""),
            false,
        );
    }
}

/// The system draws a light non-client frame around popup menus. Clip the
/// menu window to its client area so only the dark owner-drawn content shows.
fn trim_menu_frame() -> bool {
    unsafe {
        let class = wide("#32768");
        let Ok(menu) = FindWindowW(pcwstr(&class), PCWSTR::null()) else {
            return false;
        };
        if !IsWindowVisible(menu).as_bool() {
            return false;
        }
        let mut wr = RECT::default();
        let mut cr = RECT::default();
        if GetWindowRect(menu, &mut wr).is_err() || GetClientRect(menu, &mut cr).is_err() {
            return false;
        }
        let mut origin = POINT { x: 0, y: 0 };
        let _ = ClientToScreen(menu, &mut origin);
        let left = origin.x - wr.left;
        let top = origin.y - wr.top;
        let rgn = CreateRectRgn(left, top, left + (cr.right - cr.left), top + (cr.bottom - cr.top));
        // The window takes ownership of the region on success.
        SetWindowRgn(menu, Some(rgn), true);
        true
    }
}

fn open_tray_menu() {
    let Some((msg_hwnd,)) = with_app(|app| (app.msg_hwnd,)) else {
        return;
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        // Font lives until the menu is dismissed; WM_MEASUREITEM/WM_DRAWITEM
        // for the menu arrive while TrackPopupMenu runs.
        MENU_FONT.with(|f| *f.borrow_mut() = Some(segoe_font(GetDpiForSystem(), 9, 400)));
        for id in [IDM_FULLSCREEN, IDM_AUTOSTART] {
            let _ = AppendMenuW(menu, MF_OWNERDRAW, id as usize, PCWSTR::null());
        }
        let _ = AppendMenuW(menu, MF_OWNERDRAW, 0, PCWSTR::null());
        for id in [IDM_SETTINGS, IDM_QUIT] {
            let _ = AppendMenuW(menu, MF_OWNERDRAW, id as usize, PCWSTR::null());
        }

        let pos = cursor_pos();
        let _ = SetForegroundWindow(msg_hwnd);
        let _ = SetTimer(Some(msg_hwnd), IDT_MENUFRAME, 10, None);
        let _ = TrackPopupMenu(
            menu,
            TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            pos.x,
            pos.y,
            Some(0),
            msg_hwnd,
            None,
        );
        let _ = KillTimer(Some(msg_hwnd), IDT_MENUFRAME);
        let _ = PostMessageW(Some(msg_hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        MENU_FONT.with(|f| {
            if let Some(font) = f.borrow_mut().take() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        });
    }
}

fn handle_menu(id: u16) {
    match id {
        IDM_FULLSCREEN => {
            with_app(|app| {
                app.settings.disable_when_fullscreen = !app.settings.disable_when_fullscreen;
                save_settings(&app.settings, &app.settings_path);
            });
        }
        IDM_AUTOSTART => {
            with_app(|app| {
                set_auto_run(!is_auto_run(&app.exe_path), &app.exe_path);
            });
        }
        IDM_SETTINGS => open_settings_dialog(),
        IDM_QUIT => quit(),
        _ => {}
    }
}

fn quit() {
    restore_small_cursor();
    with_app(|app| {
        remove_tray_icon(app);
        if app.tray_icon_owned {
            unsafe {
                let _ = DestroyIcon(app.tray_icon);
            }
        }
        match app.image {
            CursorImage::Arrow => {}
            CursorImage::GdiPlus { image, .. } => unsafe {
                let _ = GdipDisposeImage(image);
            },
            CursorImage::Icon {
                handle, is_cursor, ..
            } => unsafe {
                if is_cursor {
                    let _ = DestroyCursor(HCURSOR(handle.0));
                } else {
                    let _ = DestroyIcon(handle);
                }
            },
        }
        unsafe {
            GdiplusShutdown(app.gdiplus_token);
        }
    });
    unsafe { PostQuitMessage(0) };
}

// ---------------------------------------------------------------- settings dialog
//
// Flat, dark, custom-drawn UI (plain GDI): Segoe UI text, rounded accent
// buttons, borderless dark edit fields, dark title bar via DWM.

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

// Palette (96 dpi layout below is scaled by the window DPI).
fn ui_bg() -> COLORREF {
    rgb(32, 32, 32)
}
fn ui_text() -> COLORREF {
    rgb(232, 232, 232)
}
fn ui_edit_bg() -> COLORREF {
    rgb(45, 45, 45)
}
fn ui_edit_border() -> COLORREF {
    rgb(74, 74, 74)
}
fn ui_accent() -> COLORREF {
    rgb(0, 120, 212)
}
fn ui_accent_pressed() -> COLORREF {
    rgb(0, 90, 158)
}
fn ui_btn_bg() -> COLORREF {
    rgb(58, 58, 58)
}
fn ui_btn_bg_pressed() -> COLORREF {
    rgb(46, 46, 46)
}
fn ui_btn_border() -> COLORREF {
    rgb(90, 90, 90)
}

const UI_W: i32 = 384;
const UI_H: i32 = 244;
const UI_MARGIN: i32 = 24;
const UI_HEAD_Y: i32 = 20;
const UI_ROW1_Y: i32 = 70;
const UI_ROW2_Y: i32 = 112;
const UI_EDIT_W: i32 = 106;
const UI_EDIT_H: i32 = 30;
const UI_BTN_Y: i32 = 194;
const UI_BTN_W: i32 = 88;
const UI_BTN_H: i32 = 30;
const UI_BTN_GAP: i32 = 8;

struct SettingsUi {
    dpi: u32,
    font: HFONT,
    font_heading: HFONT,
    br_bg: HBRUSH,
    br_edit: HBRUSH,
}

impl SettingsUi {
    fn s(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }
}

fn settings_ui(hwnd: HWND) -> Option<&'static SettingsUi> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const SettingsUi;
    if p.is_null() {
        None
    } else {
        Some(unsafe { &*p })
    }
}

fn segoe_font(dpi: u32, pt: i32, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            -(pt * dpi as i32 / 72),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            pcwstr(&face),
        )
    }
}

fn edit_rect(ui: &SettingsUi, row_y: i32) -> RECT {
    let w = ui.s(UI_EDIT_W);
    RECT {
        left: ui.s(UI_W - UI_MARGIN) - w,
        top: ui.s(row_y),
        right: ui.s(UI_W - UI_MARGIN),
        bottom: ui.s(row_y) + ui.s(UI_EDIT_H),
    }
}

fn label_rect(ui: &SettingsUi, row_y: i32) -> RECT {
    RECT {
        left: ui.s(UI_MARGIN),
        top: ui.s(row_y),
        right: ui.s(UI_W - UI_MARGIN - UI_EDIT_W - 12),
        bottom: ui.s(row_y) + ui.s(UI_EDIT_H),
    }
}

fn button_rect(ui: &SettingsUi, index: i32) -> RECT {
    // Right-aligned row of UI_BTN_W buttons: index 0 = leftmost.
    let right = ui.s(UI_W - UI_MARGIN) - (2 - index) * (ui.s(UI_BTN_W) + ui.s(UI_BTN_GAP));
    RECT {
        left: right - ui.s(UI_BTN_W),
        top: ui.s(UI_BTN_Y),
        right,
        bottom: ui.s(UI_BTN_Y) + ui.s(UI_BTN_H),
    }
}

fn draw_text(hdc: HDC, font: HFONT, color: COLORREF, rc: &mut RECT, text: &str, center: bool) {
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let format = if center {
            DT_CENTER | DT_VCENTER | DT_SINGLELINE
        } else {
            DT_LEFT | DT_VCENTER | DT_SINGLELINE
        };
        let _ = DrawTextW(hdc, &mut buf, rc, format);
        SelectObject(hdc, old);
    }
}

unsafe fn paint_settings(hwnd: HWND, ui: &SettingsUi) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.is_invalid() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);

    // Double buffer so the custom paint never flickers.
    let memdc = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(memdc, HGDIOBJ(bmp.0));

    FillRect(memdc, &rc, ui.br_bg);

    let mut head = RECT {
        left: ui.s(UI_MARGIN),
        top: ui.s(UI_HEAD_Y),
        right: rc.right - ui.s(UI_MARGIN),
        bottom: ui.s(UI_HEAD_Y) + ui.s(26),
    };
    draw_text(
        memdc,
        ui.font_heading,
        ui_text(),
        &mut head,
        "Shake detection",
        false,
    );
    for (row, text) in [
        (UI_ROW1_Y, "Corner max interval, ms (20-800)"),
        (UI_ROW2_Y, "Corner min distance, px (10-2000)"),
    ] {
        draw_text(memdc, ui.font, ui_text(), &mut label_rect(ui, row), text, false);
    }

    // Thin rounded border behind each edit field.
    let pen = CreatePen(PS_SOLID, 1, ui_edit_border());
    let old_pen = SelectObject(memdc, HGDIOBJ(pen.0));
    let old_brush = SelectObject(memdc, GetStockObject(NULL_BRUSH));
    for row in [UI_ROW1_Y, UI_ROW2_Y] {
        let r = edit_rect(ui, row);
        let i = ui.s(2);
        let _ = RoundRect(
            memdc,
            r.left - i,
            r.top - i,
            r.right + i,
            r.bottom + i,
            ui.s(8),
            ui.s(8),
        );
    }
    SelectObject(memdc, old_brush);
    SelectObject(memdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(pen.0));

    let _ = BitBlt(hdc, 0, 0, w, h, Some(memdc), 0, 0, SRCCOPY);
    SelectObject(memdc, old);
    let _ = DeleteObject(HGDIOBJ(bmp.0));
    let _ = DeleteDC(memdc);
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn draw_settings_button(ui: &SettingsUi, dis: &DRAWITEMSTRUCT) {
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let pressed = (dis.itemState.0 & ODS_SELECTED.0) != 0;
    let focused = (dis.itemState.0 & ODS_FOCUS.0) != 0;
    let is_ok = dis.CtlID == IDOK.0 as u32;

    let fill = if is_ok {
        if pressed {
            ui_accent_pressed()
        } else {
            ui_accent()
        }
    } else if pressed {
        ui_btn_bg_pressed()
    } else {
        ui_btn_bg()
    };
    let border = if is_ok {
        fill
    } else if focused {
        ui_accent()
    } else {
        ui_btn_border()
    };

    // Paint the corners outside the rounded rect with the window background,
    // otherwise the button's default (white) background shows through.
    let bg_brush = CreateSolidBrush(ui_bg());
    FillRect(hdc, &rc, bg_brush);
    let _ = DeleteObject(HGDIOBJ(bg_brush.0));

    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
    let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
    let r = ui.s(6);
    let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, r, r);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
    let _ = DeleteObject(HGDIOBJ(pen.0));
    let _ = DeleteObject(HGDIOBJ(brush.0));

    let mut text = [0u16; 64];
    let n = GetWindowTextW(dis.hwndItem, &mut text);
    let label = String::from_utf16_lossy(&text[..n.max(0) as usize]);
    let mut trc = rc;
    draw_text(hdc, ui.font, ui_text(), &mut trc, &label, true);
}

fn open_settings_dialog() {
    let existing = with_app(|app| app.settings_hwnd);
    if let Some(hwnd) = existing {
        if !hwnd.is_invalid() && unsafe { IsWindow(Some(hwnd)).as_bool() } {
            unsafe {
                let _ = SetForegroundWindow(hwnd);
            }
            return;
        }
    }

    let (gap, dist) = with_app(|app| (app.settings.gap_ms as i32, app.settings.dist_px as i32))
        .unwrap_or((120, 150));

    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let dpi = GetDpiForSystem();
        let sc = |v: i32| v * dpi as i32 / 96;
        // Center on the primary monitor work area.
        let mut area = RECT {
            right: 800,
            bottom: 600,
            ..Default::default()
        };
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut area as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        // UI_W/UI_H describe the CLIENT area; grow to the outer size so the
        // caption and frame do not eat into the button row.
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut outer = RECT {
            left: 0,
            top: 0,
            right: sc(UI_W),
            bottom: sc(UI_H),
        };
        let _ = AdjustWindowRectExForDpi(&mut outer, style, false, WS_EX_DLGMODALFRAME, dpi);
        let wpx = outer.right - outer.left;
        let hpx = outer.bottom - outer.top;
        let x = area.left + (area.right - area.left - wpx) / 2;
        let y = area.top + (area.bottom - area.top - hpx) / 2;

        let values = Box::new((gap, dist));
        let class = wide(DIALOG_CLASS);
        let title = wide("CursorFinder settings");
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_TOPMOST,
            pcwstr(&class),
            pcwstr(&title),
            style,
            x,
            y,
            wpx,
            hpx,
            None,
            None,
            Some(hinst),
            Some(Box::into_raw(values) as *const core::ffi::c_void),
        )
        .unwrap_or_default();
        if hwnd.is_invalid() {
            return;
        }
        // Dark title bar (ignored on Windows versions without support).
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        with_app(|app| app.settings_hwnd = hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn dialog_apply(hwnd: HWND) -> bool {
    let mut gap_buf = [0u16; 16];
    let mut dist_buf = [0u16; 16];
    unsafe {
        let Ok(edit_gap) = GetDlgItem(Some(hwnd), ID_EDIT_GAP) else {
            return false;
        };
        let Ok(edit_dist) = GetDlgItem(Some(hwnd), ID_EDIT_DIST) else {
            return false;
        };
        GetWindowTextW(edit_gap, &mut gap_buf);
        GetWindowTextW(edit_dist, &mut dist_buf);
    }
    let parse = |buf: &[u16; 16]| -> Option<i64> {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end]).trim().parse().ok()
    };
    let (Some(gap), Some(dist)) = (parse(&gap_buf), parse(&dist_buf)) else {
        return false;
    };
    with_app(|app| {
        app.settings.gap_ms = gap.clamp(GAP_MS_RANGE.0, GAP_MS_RANGE.1);
        app.settings.dist_px = dist.clamp(DIST_PX_RANGE.0, DIST_PX_RANGE.1);
        app.detector.gap_ms = app.settings.gap_ms as u64;
        app.detector.min_dist = app.settings.dist_px as f64;
        save_settings(&app.settings, &app.settings_path);
    });
    true
}

unsafe extern "system" fn settings_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = lparam.0 as *const CREATESTRUCTW;
            let values = if cs.is_null() {
                Box::new((120, 150))
            } else {
                let ptr = (*cs).lpCreateParams as *mut (i32, i32);
                if ptr.is_null() {
                    Box::new((120, 150))
                } else {
                    unsafe { Box::from_raw(ptr) }
                }
            };
            let (gap, dist) = *values;

            let dpi = GetDpiForWindow(hwnd);
            let ui = Box::new(SettingsUi {
                dpi,
                font: segoe_font(dpi, 9, 400),
                font_heading: segoe_font(dpi, 11, FW_SEMIBOLD.0 as i32),
                br_bg: CreateSolidBrush(ui_bg()),
                br_edit: CreateSolidBrush(ui_edit_bg()),
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(ui) as isize);
            let ui = settings_ui(hwnd).expect("settings ui");

            let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
            let edit_style = WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE(ES_NUMBER as u32)
                | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
            for (row, value, id) in [
                (UI_ROW1_Y, gap, ID_EDIT_GAP),
                (UI_ROW2_Y, dist, ID_EDIT_DIST),
            ] {
                let r = edit_rect(ui, row);
                let cls = wide("EDIT");
                let txt = wide(&value.to_string());
                let edit = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    pcwstr(&cls),
                    pcwstr(&txt),
                    edit_style,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    Some(hwnd),
                    Some(HMENU(id as isize as *mut core::ffi::c_void)),
                    Some(hinst),
                    None,
                )
                .unwrap_or_default();
                if !edit.is_invalid() {
                    let _ = PostMessageW(
                        Some(edit),
                        WM_SETFONT,
                        WPARAM(ui.font.0 as usize),
                        LPARAM(1),
                    );
                    // Text padding inside the borderless field.
                    let _ = SendMessageW(
                        edit,
                        EM_SETMARGINS,
                        Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
                        Some(LPARAM((8 | (8 << 16)) as isize)),
                    );
                }
            }

            let btn_style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32);
            for (index, text, id) in [
                (0, "OK", IDOK.0),
                (1, "Apply", ID_BTN_APPLY),
                (2, "Cancel", IDCANCEL.0),
            ] {
                let r = button_rect(ui, index);
                let cls = wide("BUTTON");
                let txt = wide(text);
                let btn = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    pcwstr(&cls),
                    pcwstr(&txt),
                    btn_style,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    Some(hwnd),
                    Some(HMENU(id as isize as *mut core::ffi::c_void)),
                    Some(hinst),
                    None,
                )
                .unwrap_or_default();
                if !btn.is_invalid() {
                    let _ = PostMessageW(Some(btn), WM_SETFONT, WPARAM(ui.font.0 as usize), LPARAM(1));
                }
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            if let Some(ui) = settings_ui(hwnd) {
                SetTextColor(hdc, ui_text());
                SetBkColor(hdc, ui_edit_bg());
                return LRESULT(ui.br_edit.0 as isize);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(ui) = settings_ui(hwnd) {
                paint_settings(hwnd, ui);
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_DRAWITEM => {
            let dis = lparam.0 as *const DRAWITEMSTRUCT;
            if !dis.is_null() && (*dis).CtlType == ODT_BUTTON {
                if let Some(ui) = settings_ui(hwnd) {
                    draw_settings_button(ui, &*dis);
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_COMMAND => {
            let id = loword(wparam.0) as i32;
            match id {
                x if x == IDOK.0 => {
                    dialog_apply(hwnd);
                    with_app(|app| app.settings_hwnd = HWND::default());
                    let _ = DestroyWindow(hwnd);
                }
                x if x == ID_BTN_APPLY => {
                    dialog_apply(hwnd);
                }
                x if x == IDCANCEL.0 => {
                    with_app(|app| app.settings_hwnd = HWND::default());
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            with_app(|app| app.settings_hwnd = HWND::default());
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            with_app(|app| app.settings_hwnd = HWND::default());
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsUi;
            if !p.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let ui = Box::from_raw(p);
                let _ = DeleteObject(HGDIOBJ(ui.font.0));
                let _ = DeleteObject(HGDIOBJ(ui.font_heading.0));
                let _ = DeleteObject(HGDIOBJ(ui.br_bg.0));
                let _ = DeleteObject(HGDIOBJ(ui.br_edit.0));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------- wndprocs

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_SETCURSOR {
        // Hide the real cursor while it is over the overlay (same as the
        // original's Qt::BlankCursor).
        let _ = SetCursor(None);
        return LRESULT(1);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn msg_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_created = with_app(|app| app.taskbar_created_msg).unwrap_or(0);

    if msg == WM_TIMER {
        match wparam.0 {
            IDT_DETECT => {
                with_app(detect_tick);
            }
            IDT_FOLLOW => {
                with_app(follow_tick);
            }
            IDT_MENUFRAME if trim_menu_frame() => unsafe {
                let _ = KillTimer(Some(hwnd), IDT_MENUFRAME);
            },
            _ => {}
        }
        return LRESULT(0);
    }

    if msg == WM_TRAY {
        let event = loword(lparam.0 as usize) as u32;
        if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
            open_tray_menu();
        }
        return LRESULT(0);
    }

    if msg == WM_MEASUREITEM {
        let mis = lparam.0 as *mut MEASUREITEMSTRUCT;
        if !mis.is_null() && unsafe { (*mis).CtlType } == ODT_MENU {
            unsafe { measure_menu_item(&mut *mis) };
            return LRESULT(1);
        }
    }

    if msg == WM_DRAWITEM {
        let dis = lparam.0 as *mut DRAWITEMSTRUCT;
        if !dis.is_null() && unsafe { (*dis).CtlType } == ODT_MENU {
            unsafe { draw_menu_item(&*dis) };
            return LRESULT(1);
        }
    }

    if msg == WM_COMMAND {
        handle_menu(loword(wparam.0));
        return LRESULT(0);
    }

    if taskbar_created != 0 && msg == taskbar_created {
        // Explorer restarted -> re-add the tray icon.
        with_app(|app| add_tray_icon(app, false));
        return LRESULT(0);
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

// ---------------------------------------------------------------- entry

fn register_class(name: &str, proc: WNDPROC) -> bool {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let class_name = wide(name);
        let wc = WNDCLASSW {
            hInstance: hinst,
            lpszClassName: pcwstr(&class_name),
            lpfnWndProc: proc,
            ..Default::default()
        };
        RegisterClassW(&wc) != 0
    }
}

pub fn run() {
    unsafe {
        // Crisper positioning on high-DPI setups (the Qt build was not DPI
        // aware and got virtualized coordinates).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let mut token = 0usize;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let _ = GdiplusStartup(&mut token, &input, ptr::null_mut());

        register_class(OVERLAY_CLASS, Some(overlay_wndproc));
        register_class(MSG_CLASS, Some(msg_wndproc));
        register_class(DIALOG_CLASS, Some(settings_wndproc));

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();

        // Hidden message-only window: owns the tray icon and all timers.
        let msg_class = wide(MSG_CLASS);
        let msg_hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            pcwstr(&msg_class),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        )
        .expect("failed to create message window");

        // Layered overlay for the enlarged cursor.
        let overlay_class = wide(OVERLAY_CLASS);
        let overlay_hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            pcwstr(&overlay_class),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            MAX_CURSOR_WIDTH as i32,
            MAX_CURSOR_WIDTH as i32,
            None,
            None,
            Some(hinst),
            None,
        )
        .expect("failed to create overlay window");

        let exe = exe_path();
        let exe_dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let settings_path = exe_dir.join("settings.ini");
        let settings = load_settings(&settings_path);
        let detector = ShakeDetector::new(settings.gap_ms as u64, settings.dist_px as f64);

        let (tray_icon, tray_icon_owned) = tray_icon_handle();
        let app = App {
            msg_hwnd,
            overlay_hwnd,
            settings_hwnd: HWND::default(),
            detector,
            settings,
            settings_path,
            exe_path: exe,
            image: load_cursor_image(&exe_dir),
            gdiplus_token: token,
            taskbar_created_msg: RegisterWindowMessageW(pcwstr(&wide("TaskbarCreated"))),
            visible: false,
            anim_t: 0.0,
            visibility: ShakeVisibility::default(),
            tray_icon,
            tray_icon_owned,
        };

        add_tray_icon(&app, true);
        APP.with(|a| *a.borrow_mut() = Some(app));

        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let _ = SetTimer(Some(msg_hwnd), IDT_DETECT, DETECT_INTERVAL_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
