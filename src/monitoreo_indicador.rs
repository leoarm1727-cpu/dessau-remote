

#![cfg(windows)]

use hbb_common::log;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use winapi::shared::minwindef::{HINSTANCE, LPARAM, LRESULT, TRUE, UINT, WPARAM};
use winapi::shared::windef::{HBRUSH, HDC, HFONT, HGDIOBJ, HWND, RECT, SIZE};
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::wingdi::{
    CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, GetTextExtentPoint32W,
    SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, FF_DONTCARE, FW_SEMIBOLD, OUT_TT_PRECIS, TRANSPARENT,
};
use winapi::um::winuser::{
    BeginPaint, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawTextW,
    EndPaint, FillRect, GetClientRect, GetDC, GetMessageW, GetSystemMetrics, InvalidateRect,
    KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, ReleaseDC,
    SetLayeredWindowAttributes, SetTimer, SetWindowPos, SetWindowRgn, ShowWindow, TranslateMessage,
    UpdateWindow, DT_LEFT, DT_NOCLIP, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HWND_TOPMOST,
    IDC_ARROW, LWA_ALPHA, MSG, PAINTSTRUCT, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_PAINT,
    WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASE: &str = "DessauMonitoreoIndicador";

const ERROR_CLASS_ALREADY_EXISTS: i32 = 1410;
const TIMER_TOPMOST: usize = 1;

const NAVY: (u8, u8, u8) = (15, 60, 99);
const TEAL: (u8, u8, u8) = (8, 153, 139);
const GOLD: (u8, u8, u8) = (240, 190, 30);
const BLANCO: (u8, u8, u8) = (255, 255, 255);
const TENUE: (u8, u8, u8) = (125, 158, 186);

const TRAMOS: [(&str, (u8, u8, u8), i32); 4] = [
    ("\u{25CF}", TEAL, 0),
    ("Monitoreo de productividad activo", BLANCO, 9),
    ("\u{2014}", TENUE, 10),
    ("DESSAU S&Z", GOLD, 10),
];

const ALTO: i32 = 34;
const BARRA: i32 = 5;
const PAD: i32 = 14;
const PAD_DER: i32 = 16;
const ANCHO_FALLBACK: i32 = 430;
const MARGEN: i32 = 16;
const GAP_TASKBAR: i32 = 52;
const RADIO: i32 = 12;
const ALPHA: u8 = 235;

pub struct IndicadorHandle {

    hwnd: Arc<AtomicI64>,
    hilo: Option<JoinHandle<()>>,
}

impl IndicadorHandle {

    pub fn detener(mut self) {
        self.parar();
    }

    fn parar(&mut self) {
        if let Some(hilo) = self.hilo.take() {

            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {

                unsafe {
                    PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }
            let _ = hilo.join();
        }
    }
}

impl Drop for IndicadorHandle {

    fn drop(&mut self) {
        self.parar();
    }
}

pub fn iniciar_indicador() -> IndicadorHandle {
    let (tx, rx) = std::sync::mpsc::channel::<isize>();
    let hwnd = Arc::new(AtomicI64::new(0));
    let hwnd_hilo = hwnd.clone();

    let hilo = match std::thread::Builder::new()
        .name("dessau-indicador".into())
        .spawn(move || unsafe {
            correr_ventana(tx, hwnd_hilo);
        }) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("[indicador] no se pudo crear el hilo del indicador: {e}");
            return IndicadorHandle { hwnd, hilo: None };
        }
    };

    let _ = rx.recv_timeout(Duration::from_secs(5));
    if hwnd.load(Ordering::SeqCst) == 0 {
        log::warn!("[indicador] no se pudo crear la ventana del indicador visible");
    } else {
        log::info!("[indicador] indicador visible de monitoreo ACTIVO");
    }

    IndicadorHandle {
        hwnd,
        hilo: Some(hilo),
    }
}

unsafe fn correr_ventana(tx: Sender<isize>, hwnd_compartido: Arc<AtomicI64>) {
    let hinst = GetModuleHandleW(std::ptr::null()) as HINSTANCE;
    registrar_clase(hinst);

    let clase = a_wide(CLASE);
    let titulo = a_wide("Dessau");
    let w = medir_ancho();
    let h = ALTO;
    let (x, y) = esquina(w, h);

    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
        clase.as_ptr(),
        titulo.as_ptr(),
        WS_POPUP,
        x,
        y,
        w,
        h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinst,
        std::ptr::null_mut(),
    );

    if hwnd.is_null() {
        log::warn!(
            "[indicador] CreateWindowExW falló: {}",
            std::io::Error::last_os_error()
        );
        let _ = tx.send(0);
        return;
    }

    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, RADIO, RADIO);
    if !rgn.is_null() && SetWindowRgn(hwnd, rgn, TRUE) == 0 {
        DeleteObject(rgn as HGDIOBJ);
    }

    SetLayeredWindowAttributes(hwnd, 0, ALPHA, LWA_ALPHA);
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    UpdateWindow(hwnd);

    SetTimer(hwnd, TIMER_TOPMOST, 2000, None);

    hwnd_compartido.store(hwnd as i64, Ordering::SeqCst);
    let _ = tx.send(hwnd as isize);

    let mut msg: MSG = std::mem::zeroed();

    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    hwnd_compartido.store(0, Ordering::SeqCst);
}

unsafe fn registrar_clase(hinst: HINSTANCE) {
    let clase = a_wide(CLASE);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as UINT,
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: std::ptr::null_mut(),
        hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: clase.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    if RegisterClassExW(&wc) == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_CLASS_ALREADY_EXISTS) {

            log::warn!("[indicador] RegisterClassExW: {err}");
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            pintar(hwnd);
            0
        }
        WM_TIMER => {

            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            0
        }
        WM_DISPLAYCHANGE => {

            let mut rc: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut rc) != 0 {
                let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
                let (x, y) = esquina(w, h);
                SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            InvalidateRect(hwnd, std::ptr::null(), TRUE);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_TOPMOST);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn pintar(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc: HDC = BeginPaint(hwnd, &mut ps);
    if hdc.is_null() {
        return;
    }

    let mut rc: RECT = std::mem::zeroed();
    if GetClientRect(hwnd, &mut rc) == 0 {
        EndPaint(hwnd, &ps);
        return;
    }

    let fondo: HBRUSH = CreateSolidBrush(color(NAVY));
    if !fondo.is_null() {
        FillRect(hdc, &rc, fondo);
        DeleteObject(fondo as HGDIOBJ);
    }

    let mut rb = rc;
    rb.right = rb.left + BARRA;
    let franja: HBRUSH = CreateSolidBrush(color(TEAL));
    if !franja.is_null() {
        FillRect(hdc, &rb, franja);
        DeleteObject(franja as HGDIOBJ);
    }

    let font: HFONT = crear_fuente();
    let old: HGDIOBJ = if font.is_null() {
        std::ptr::null_mut()
    } else {
        SelectObject(hdc, font as HGDIOBJ)
    };

    SetBkMode(hdc, TRANSPARENT as i32);
    let fmt = DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX | DT_NOCLIP;

    let mut x = BARRA + PAD;
    for (texto, col, gap) in TRAMOS.iter() {
        x += gap;
        let w = a_wide(texto);
        let mut rt = rc;
        rt.left = x;
        SetTextColor(hdc, color(*col));
        DrawTextW(hdc, w.as_ptr(), -1, &mut rt, fmt);
        x += ancho_texto(hdc, &w);
    }

    if !old.is_null() {
        SelectObject(hdc, old);
    }
    if !font.is_null() {
        DeleteObject(font as HGDIOBJ);
    }

    EndPaint(hwnd, &ps);
}

unsafe fn crear_fuente() -> HFONT {
    let face = a_wide("Segoe UI");
    CreateFontW(
        -18,
        0,
        0,
        0,
        FW_SEMIBOLD,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_TT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        face.as_ptr(),
    )
}

unsafe fn ancho_texto(hdc: HDC, w: &[u16]) -> i32 {
    let n = w.len().saturating_sub(1) as i32;
    if n <= 0 {
        return 0;
    }
    let mut s: SIZE = std::mem::zeroed();
    if GetTextExtentPoint32W(hdc, w.as_ptr(), n, &mut s) == 0 {
        0
    } else {
        s.cx
    }
}

unsafe fn medir_ancho() -> i32 {
    let hdc = GetDC(std::ptr::null_mut());
    if hdc.is_null() {
        return ANCHO_FALLBACK;
    }
    let font = crear_fuente();
    let old: HGDIOBJ = if font.is_null() {
        std::ptr::null_mut()
    } else {
        SelectObject(hdc, font as HGDIOBJ)
    };

    let mut total = BARRA + PAD + PAD_DER;
    for (texto, _, gap) in TRAMOS.iter() {
        total += gap + ancho_texto(hdc, &a_wide(texto));
    }

    if !old.is_null() {
        SelectObject(hdc, old);
    }
    if !font.is_null() {
        DeleteObject(font as HGDIOBJ);
    }
    ReleaseDC(std::ptr::null_mut(), hdc);

    if total < 200 {
        ANCHO_FALLBACK
    } else {
        total
    }
}

unsafe fn esquina(ancho: i32, alto: i32) -> (i32, i32) {
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    (
        (sw - ancho - MARGEN).max(0),
        (sh - alto - GAP_TASKBAR).max(0),
    )
}

fn color((r, g, b): (u8, u8, u8)) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

fn a_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
