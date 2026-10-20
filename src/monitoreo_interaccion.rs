

#![cfg(windows)]

use hbb_common::log;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use winapi::shared::minwindef::{HINSTANCE, LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::HWND;
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::sysinfoapi::GetTickCount;
use winapi::um::winuser::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetRawInputData, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterRawInputDevices,
    TranslateMessage, HWND_MESSAGE, MSG, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK,
    MOUSE_MOVE_ABSOLUTE, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RI_KEY_BREAK,
    RI_MOUSE_BUTTON_4_DOWN,
    RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_DOWN,
    RI_MOUSE_RIGHT_BUTTON_DOWN, WM_CLOSE, WM_DESTROY, WM_INPUT, WNDCLASSEXW,
};

const CLASE: &str = "DessauMonitoreoInteraccion";

static PULSACIONES: AtomicU64 = AtomicU64::new(0);
static CLICS: AtomicU64 = AtomicU64::new(0);
static SEG_MOVIMIENTO: AtomicU64 = AtomicU64::new(0);

static ULTIMO_SEG_MOV: AtomicU64 = AtomicU64::new(u64::MAX);

const BOTON_ABAJO: u16 = RI_MOUSE_LEFT_BUTTON_DOWN
    | RI_MOUSE_RIGHT_BUTTON_DOWN
    | RI_MOUSE_MIDDLE_BUTTON_DOWN
    | RI_MOUSE_BUTTON_4_DOWN
    | RI_MOUSE_BUTTON_5_DOWN;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Contadores {
    pub pulsaciones: u64,
    pub clics: u64,
    pub seg_movimiento: u64,
}

impl Contadores {
    pub fn sumar(&mut self, o: Contadores) {
        self.pulsaciones = self.pulsaciones.saturating_add(o.pulsaciones);
        self.clics = self.clics.saturating_add(o.clics);
        self.seg_movimiento = self.seg_movimiento.saturating_add(o.seg_movimiento);
    }
}

pub fn tomar() -> Contadores {
    Contadores {
        pulsaciones: PULSACIONES.swap(0, Ordering::Relaxed),
        clics: CLICS.swap(0, Ordering::Relaxed),
        seg_movimiento: SEG_MOVIMIENTO.swap(0, Ordering::Relaxed),
    }
}

pub struct ContadorHandle {

    hwnd: Arc<AtomicI64>,
    hilo: Option<JoinHandle<()>>,
}

impl ContadorHandle {

    pub fn activo(&self) -> bool {
        self.hwnd.load(Ordering::SeqCst) != 0
    }

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
        let _ = tomar();
    }
}

impl Drop for ContadorHandle {
    fn drop(&mut self) {
        self.parar();
    }
}

pub fn iniciar_contador() -> ContadorHandle {
    let (tx, rx) = std::sync::mpsc::channel::<isize>();
    let hwnd = Arc::new(AtomicI64::new(0));
    let hwnd_hilo = hwnd.clone();

    let hilo = match std::thread::Builder::new()
        .name("dessau-interaccion".into())
        .spawn(move || unsafe {
            correr_ventana(tx, hwnd_hilo);
        }) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("[interaccion] no se pudo crear el hilo del contador: {e}");
            return ContadorHandle { hwnd, hilo: None };
        }
    };

    let _ = rx.recv_timeout(Duration::from_secs(5));
    if hwnd.load(Ordering::SeqCst) == 0 {
        log::warn!("[interaccion] el contador de interacción no arrancó: se reportará NULL");
    } else {
        log::info!("[interaccion] contadores agregados ACTIVOS (solo totales)");
    }

    ContadorHandle {
        hwnd,
        hilo: Some(hilo),
    }
}

unsafe fn correr_ventana(tx: Sender<isize>, hwnd_compartido: Arc<AtomicI64>) {
    let hinst = GetModuleHandleW(std::ptr::null()) as HINSTANCE;
    registrar_clase(hinst);

    let clase = a_wide(CLASE);
    let titulo = a_wide("Dessau");

    let hwnd = CreateWindowExW(
        0,
        clase.as_ptr(),
        titulo.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        HWND_MESSAGE,
        std::ptr::null_mut(),
        hinst,
        std::ptr::null_mut(),
    );
    if hwnd.is_null() {
        let _ = tx.send(0);
        return;
    }

    hwnd_compartido.store(hwnd as i64, Ordering::SeqCst);

    let devs = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
    ];
    let ok = RegisterRawInputDevices(
        devs.as_ptr(),
        devs.len() as u32,
        std::mem::size_of::<RAWINPUTDEVICE>() as u32,
    );
    if ok == 0 {
        log::warn!("[interaccion] RegisterRawInputDevices falló; no habrá contadores");
        hwnd_compartido.store(0, Ordering::SeqCst);
        DestroyWindow(hwnd);
        let _ = tx.send(0);
        return;
    }

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
    let mut wc: WNDCLASSEXW = std::mem::zeroed();
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinst;
    wc.lpszClassName = clase.as_ptr();

    RegisterClassExW(&wc);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            contar(lp);

            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn contar(lp: LPARAM) {
    let mut raw: RAWINPUT = std::mem::zeroed();
    let mut tam = std::mem::size_of::<RAWINPUT>() as u32;
    let leidos = GetRawInputData(
        lp as _,
        RID_INPUT,
        &mut raw as *mut RAWINPUT as *mut _,
        &mut tam,
        std::mem::size_of::<RAWINPUTHEADER>() as u32,
    );

    if leidos == 0 || leidos == u32::MAX {
        return;
    }

    match raw.header.dwType {
        RIM_TYPEKEYBOARD => {
            let kb = raw.data.keyboard();

            if (kb.Flags as u32 & RI_KEY_BREAK) == 0 {
                PULSACIONES.fetch_add(1, Ordering::Relaxed);
            }
        }
        RIM_TYPEMOUSE => {
            let m = raw.data.mouse();
            if (m.usButtonFlags & BOTON_ABAJO) != 0 {
                CLICS.fetch_add(1, Ordering::Relaxed);
            }

            let se_movio = if (m.usFlags & MOUSE_MOVE_ABSOLUTE) != 0 {
                m.usButtonFlags == 0
            } else {
                m.lLastX != 0 || m.lLastY != 0
            };
            if se_movio {
                let seg = (GetTickCount() as u64) / 1000;

                if ULTIMO_SEG_MOV.swap(seg, Ordering::Relaxed) != seg {
                    SEG_MOVIMIENTO.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
}

fn a_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
