// =============================================================================
//  Indicador VISIBLE del monitoreo (Ley 29733) — banner permanente en pantalla
// -----------------------------------------------------------------------------
//  ⚖️ Esto NO oculta nada: al contrario, lo MUESTRA. Es la señal que el trabajador
//  SIEMPRE ve mientras el monitoreo de productividad está activo. Es lo que, junto
//  al consentimiento (monitoreo_aviso.rs), hace LEGÍTIMA la medición/captura: el
//  empleado está informado y ve, en todo momento, que el seguimiento está encendido.
//
//  Diseño (Opción A): una ventana pequeña, sin bordes, siempre-encima, anclada en
//  la esquina inferior derecha, con la identidad Dessau S&Z (azul institucional
//  #0F3C63, franja teal #08998B, marca en dorado #F0BE1E) y el texto "● Monitoreo
//  de productividad activo — DESSAU S&Z". No roba foco (WS_EX_NOACTIVATE), no
//  aparece en la barra de tareas (WS_EX_TOOLWINDOW), y se reafirma "topmost" con un
//  timer por si otra app se pone encima. Vive en su PROPIO hilo con message loop
//  (GetMessageW/DispatchMessageW): las ventanas Win32 entregan sus mensajes al hilo
//  que las creó, así que el loop DEBE correr ahí, no en el hilo del agente (que se
//  bloquea en HTTP). Se crea al encender el monitoreo y se cierra al apagarlo
//  (si TI apaga el seguimiento, se oculta).
//
//  NO SE PUEDE PAUSAR: el banner es CLICK-THROUGH (WS_EX_TRANSPARENT). No recibe
//  clics, no se mueve, no se cierra y ya no ofrece "tiempo privado" — mientras el
//  monitoreo esté encendido el aviso está a la vista y el trabajador no puede
//  apagarlo. Encender o apagar el monitoreo es potestad de TI (política del
//  servidor), no del equipo.
//
//  Corre en el proceso `--tray` (sesión interactiva del usuario; ver core_main.rs).
//  Reusa SOLO winapi 0.3 (ya dep). Patrón de creación/loop adaptado de
//  src/privacy_mode/win_input.rs y libs/scrap/src/dxgi/mag.rs (ver README del fork).
//
//  ⚠️ El perfil release del fork usa `panic = 'abort'` (Cargo.toml): un panic acá
//  tumbaría el proceso --tray. Todas las rutas están guardadas — NO introducir
//  unwrap()/expect() ni indexado sin guarda en este módulo.
//  ⚠️ AGPL-3.0: es una modificación del cliente RustDesk. Sin secretos horneados.
// =============================================================================
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

/// Nombre de la clase de ventana. Se registra UNA vez por proceso (ver `registrar_clase`).
const CLASE: &str = "DessauMonitoreoIndicador";
/// ERROR_CLASS_ALREADY_EXISTS: si reiniciamos el indicador la clase ya está registrada.
const ERROR_CLASS_ALREADY_EXISTS: i32 = 1410;
const TIMER_TOPMOST: usize = 1;

// Paleta institucional Dessau S&Z (Manual de Marca; la misma de la app web).
const NAVY: (u8, u8, u8) = (15, 60, 99); // #0F3C63 azul corporativo (fondo)
const TEAL: (u8, u8, u8) = (8, 153, 139); // #08998B acento (franja y testigo ●)
const GOLD: (u8, u8, u8) = (240, 190, 30); // #F0BE1E dorado (marca)
const BLANCO: (u8, u8, u8) = (255, 255, 255);
const TENUE: (u8, u8, u8) = (125, 158, 186); // separador, azul claro apagado

/// Texto del banner por tramos: (texto, color, separación antes del tramo).
/// El "●" (U+25CF) en teal es el testigo de "monitoreo encendido".
const TRAMOS: [(&str, (u8, u8, u8), i32); 4] = [
    ("\u{25CF}", TEAL, 0),
    ("Monitoreo de productividad activo", BLANCO, 9),
    ("\u{2014}", TENUE, 10),
    ("DESSAU S&Z", GOLD, 10),
];

// Geometría del banner (px lógicos; ver nota de DPI en el reporte del módulo).
const ALTO: i32 = 34;
const BARRA: i32 = 5; // franja teal pegada al borde izquierdo
const PAD: i32 = 14; // separación entre la franja y el texto
const PAD_DER: i32 = 16; // aire a la derecha del último tramo
const ANCHO_FALLBACK: i32 = 430; // si no se puede medir el texto
const MARGEN: i32 = 16; // separación del borde derecho
const GAP_TASKBAR: i32 = 52; // separación del borde inferior (deja pasar la barra)
const RADIO: i32 = 12; // esquinas redondeadas (como la app)
const ALPHA: u8 = 235; // casi opaco: bien visible, con un pelín de transparencia

/// Handle para DETENER el indicador. El indicador sólo debe verse mientras el
/// monitoreo está encendido; al soltar este handle (o llamar `detener`) la ventana
/// se cierra y su hilo termina. Es `Send` (guarda el HWND como entero).
pub struct IndicadorHandle {
    /// HWND como entero, COMPARTIDO con el hilo (0 = todavía/nunca creado). Es un
    /// atómico y no una copia: si la ventana se crea DESPUÉS de que venza el
    /// handshake de 5 s, una copia en cero haría que `parar()` no mandara el
    /// WM_CLOSE y el `join()` colgara el agente para siempre (mismo arreglo que en
    /// monitoreo_interaccion.rs).
    hwnd: Arc<AtomicI64>,
    hilo: Option<JoinHandle<()>>,
}

impl IndicadorHandle {
    /// Cierra el indicador y espera a que su hilo termine. Idempotente y seguro
    /// de llamar desde cualquier hilo EXCEPTO el propio del indicador.
    pub fn detener(mut self) {
        self.parar();
    }

    fn parar(&mut self) {
        if let Some(hilo) = self.hilo.take() {
            // Se relee el HWND: si la ventana se creó tarde, acá ya está. Si vale 0,
            // el hilo terminó solo (CreateWindowExW falló) y el join no bloquea.
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                // PostMessageW es thread-safe: encola WM_CLOSE en el hilo dueño de la
                // ventana. Ese hilo hace DestroyWindow -> WM_DESTROY -> PostQuitMessage,
                // GetMessageW devuelve 0 y el loop termina limpio.
                unsafe {
                    PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }
            let _ = hilo.join();
        }
    }
}

impl Drop for IndicadorHandle {
    // Red de seguridad: si el handle se descarta sin llamar `detener`, igual se cierra.
    fn drop(&mut self) {
        self.parar();
    }
}

/// Enciende el indicador visible en su propio hilo y devuelve un handle para pararlo.
/// Nunca hace panic —ni si el sistema no puede crear el hilo: `Builder::spawn`
/// devuelve `Result`, y el release es `panic = 'abort'`—. Si la ventana no se puede
/// crear, devuelve un handle "vacío" (hwnd = 0) y lo registra en el log; el resto
/// del monitoreo puede seguir.
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

    // Esperamos (con tope) a que el hilo cree la ventana; la fuente de verdad del
    // HWND es el atómico, que el hilo escribe apenas la crea.
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

/// Cuerpo del hilo del indicador: registra la clase (una vez), crea la ventana,
/// avisa el HWND por el canal y corre el message loop hasta WM_QUIT.
unsafe fn correr_ventana(tx: Sender<isize>, hwnd_compartido: Arc<AtomicI64>) {
    let hinst = GetModuleHandleW(std::ptr::null()) as HINSTANCE;
    registrar_clase(hinst);

    let clase = a_wide(CLASE);
    let titulo = a_wide("Dessau");
    let w = medir_ancho();
    let h = ALTO;
    let (x, y) = esquina(w, h);

    // WS_EX_TRANSPARENT: click-through. El banner NO recibe clics — no se puede
    // pausar, mover ni cerrar desde el escritorio del trabajador, y nunca bloquea
    // lo que hay debajo. Sigue sin robar foco (WS_EX_NOACTIVATE) ni aparecer en la
    // barra de tareas (WS_EX_TOOLWINDOW).
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
        clase.as_ptr(),
        titulo.as_ptr(),
        WS_POPUP, // sin WS_VISIBLE: lo mostramos con SW_SHOWNOACTIVATE (no activa/roba foco)
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

    // Esquinas redondeadas. El sistema sólo adopta la región si SetWindowRgn
    // devuelve != 0; si falla, la región sigue siendo nuestra y hay que liberarla.
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, RADIO, RADIO);
    if !rgn.is_null() && SetWindowRgn(hwnd, rgn, TRUE) == 0 {
        DeleteObject(rgn as HGDIOBJ);
    }

    // Casi opaco pero un poco translúcido: bien visible sin tapar del todo lo de abajo.
    SetLayeredWindowAttributes(hwnd, 0, ALPHA, LWA_ALPHA);
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    UpdateWindow(hwnd);
    // Reafirmar topmost cada 2 s por si otra ventana "always on top" se pone encima.
    SetTimer(hwnd, TIMER_TOPMOST, 2000, None);

    // Publicamos el HWND para que iniciar_indicador() pueda pararlo luego: el
    // atómico es lo que manda; el canal solo sirve para loguear a tiempo.
    hwnd_compartido.store(hwnd as i64, Ordering::SeqCst);
    let _ = tx.send(hwnd as isize);

    // Message loop: DEBE correr en este hilo (el que creó la ventana).
    let mut msg: MSG = std::mem::zeroed();
    // GetMessageW: >0 hay mensaje, 0 es WM_QUIT (salir), -1 es error (salir también).
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    // La ventana ya no existe: que nadie le mande mensajes.
    hwnd_compartido.store(0, Ordering::SeqCst);
}

/// Registra la clase de ventana. Se llama en cada arranque del indicador; si ya
/// estaba registrada (reinicio del monitoreo) se tolera ERROR_CLASS_ALREADY_EXISTS.
/// No des-registramos la clase al cerrar: dejarla registrada es correcto y evita
/// carreras si hubiera dos indicadores.
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
        hbrBackground: std::ptr::null_mut(), // pintamos todo el fondo en WM_PAINT
        lpszMenuName: std::ptr::null(),
        lpszClassName: clase.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    if RegisterClassExW(&wc) == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_CLASS_ALREADY_EXISTS) {
            // Si es otro error, CreateWindowExW fallará después y lo reportaremos ahí.
            log::warn!("[indicador] RegisterClassExW: {err}");
        }
    }
}

/// Procedimiento de ventana. `extern "system"` + `#[no_mangle]` no es necesario
/// (se pasa por puntero en WNDCLASSEXW), pero sí la ABI del sistema. No hay
/// WM_LBUTTONUP: la ventana es click-through y el banner no se puede pausar.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            pintar(hwnd);
            0
        }
        WM_TIMER => {
            // Reafirmar el z-order "topmost" sin mover ni redimensionar ni activar.
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
            // Cambió la resolución/monitores: reubicar en la esquina y repintar.
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

/// Pinta el banner con la identidad Dessau: fondo azul institucional, franja teal
/// a la izquierda y el texto por tramos (testigo teal, aviso en blanco, marca en
/// dorado). Crea y libera sus objetos GDI en cada WM_PAINT (el banner es estático,
/// así que WM_PAINT es raro); así no hay estado por-ventana ni fugas de GDI.
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

    // Fondo azul corporativo.
    let fondo: HBRUSH = CreateSolidBrush(color(NAVY));
    if !fondo.is_null() {
        FillRect(hdc, &rc, fondo);
        DeleteObject(fondo as HGDIOBJ);
    }

    // Franja teal del borde izquierdo (acento de marca).
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

    // Tramo a tramo, avanzando la x con el ancho real de cada texto.
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

    // Restaurar y liberar la fuente (restaurar ANTES de borrar: no se puede borrar
    // un objeto que sigue seleccionado).
    if !old.is_null() {
        SelectObject(hdc, old);
    }
    if !font.is_null() {
        DeleteObject(font as HGDIOBJ);
    }

    EndPaint(hwnd, &ps);
}

/// Segoe UI semibold ~13.5pt (altura negativa = alto de carácter). CreateFontW
/// copia el nombre de la tipografía, así que el buffer local puede morir acá.
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

/// Ancho en píxeles de un texto ya convertido a UTF-16 con NUL final, en ese DC.
unsafe fn ancho_texto(hdc: HDC, w: &[u16]) -> i32 {
    let n = w.len().saturating_sub(1) as i32; // sin el NUL final
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

/// Ancho del banner medido con la tipografía real, para que el texto no se corte
/// si cambia el DPI o la fuente del sistema. Si algo falla, cae al ancho fijo.
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

/// Esquina inferior derecha del monitor primario. GetSystemMetrics da píxeles del
/// monitor primario en el contexto DPI del proceso (ver nota de DPI en el reporte).
unsafe fn esquina(ancho: i32, alto: i32) -> (i32, i32) {
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    (
        (sw - ancho - MARGEN).max(0),
        (sh - alto - GAP_TASKBAR).max(0),
    )
}

/// COLORREF (0x00BBGGRR) a partir de componentes RGB, sin depender de la macro RGB.
fn color((r, g, b): (u8, u8, u8)) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// &str -> UTF-16 terminado en NUL (para las APIs *W de Win32).
fn a_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
