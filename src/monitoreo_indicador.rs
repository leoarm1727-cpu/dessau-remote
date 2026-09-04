// =============================================================================
//  Indicador VISIBLE del monitoreo (Ley 29733) — banner permanente en pantalla
// -----------------------------------------------------------------------------
//  ⚖️ Esto NO oculta nada: al contrario, lo MUESTRA. Es la señal que el trabajador
//  SIEMPRE ve mientras el monitoreo de productividad está activo. Es lo que, junto
//  al consentimiento (monitoreo_aviso.rs), hace LEGÍTIMA la medición/captura: el
//  empleado está informado y ve, en todo momento, que el seguimiento está encendido.
//
//  Diseño (Opción A): una ventana pequeña, sin bordes, siempre-encima, anclada en
//  la esquina inferior derecha, con el texto "● Monitoreo de productividad activo
//  — Dessau". No roba foco (WS_EX_NOACTIVATE), no aparece en la barra de tareas
//  (WS_EX_TOOLWINDOW), y se reafirma "topmost" con un timer por si otra app se
//  pone encima. Vive en su PROPIO hilo con message loop (GetMessageW/
//  DispatchMessageW): las ventanas Win32 entregan sus mensajes al hilo que las
//  creó, así que el loop DEBE correr ahí, no en el hilo del agente (que se
//  bloquea en HTTP). Se crea al encender el monitoreo y se cierra al apagarlo
//  (si TI apaga el seguimiento, se oculta).
//
//  Tiempo privado (opt-in, si TI lo permite): el banner RECIBE CLICS (ya no es
//  click-through) — un clic alterna un flag compartido (Arc<AtomicBool>) que el
//  agente lee para marcar el intervalo como "privado" (sin app/título/URL, ver
//  monitoreo.rs) y cambiar el texto del banner. Se auto-reanuda solo tras el
//  tope de minutos que fije TI (`tiempo_privado_max_min`).
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
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use winapi::shared::minwindef::{HINSTANCE, LPARAM, LRESULT, TRUE, UINT, WPARAM};
use winapi::shared::windef::{HBRUSH, HDC, HFONT, HGDIOBJ, HWND, RECT};
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::wingdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, SelectObject, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE,
    FW_SEMIBOLD, OUT_TT_PRECIS, TRANSPARENT,
};
use winapi::um::winuser::{
    BeginPaint, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawTextW,
    EndPaint, FillRect, GetClientRect, GetMessageW, GetSystemMetrics, InvalidateRect, KillTimer,
    LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetLayeredWindowAttributes,
    SetTimer, SetWindowPos, ShowWindow, TranslateMessage, UpdateWindow, DT_LEFT, DT_NOCLIP,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HWND_TOPMOST, IDC_ARROW, LWA_ALPHA, MSG, PAINTSTRUCT,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_LBUTTONUP, WM_PAINT, WM_TIMER,
    WM_USER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Mensaje propio: pide repintar el banner (lo envía el agente al auto-reanudar el
/// tiempo privado desde su hilo; PostMessageW es thread-safe).
const WM_REPINTAR: UINT = WM_USER + 1;

/// Estado que comparte el banner con el agente. `privado` lo alterna el trabajador
/// (clic en el banner) y lo lee el agente; `permitido` dice si TI habilita la pausa.
struct EstadoIndicador {
    privado: Arc<AtomicBool>,
    permitido: bool,
}

thread_local! {
    // Vive en el hilo del indicador (el mismo donde corre wndproc). Se setea al crear
    // la ventana y lo leen WM_PAINT / WM_LBUTTONUP.
    static ESTADO: RefCell<Option<EstadoIndicador>> = RefCell::new(None);
}

/// Texto permanente del indicador. El "●" (U+25CF) se repinta en verde encima.
const TEXTO: &str = "\u{25CF} Monitoreo de productividad activo \u{2014} Dessau";
/// Nombre de la clase de ventana. Se registra UNA vez por proceso (ver `registrar_clase`).
const CLASE: &str = "DessauMonitoreoIndicador";
/// ERROR_CLASS_ALREADY_EXISTS: si reiniciamos el indicador la clase ya está registrada.
const ERROR_CLASS_ALREADY_EXISTS: i32 = 1410;
const TIMER_TOPMOST: usize = 1;

// Geometría del banner (px lógicos; ver nota de DPI en el reporte del módulo).
const ANCHO: i32 = 430; // ancho para el texto más largo del hint de pausa privada
const ALTO: i32 = 34;
const MARGEN: i32 = 16; // separación del borde derecho
const GAP_TASKBAR: i32 = 52; // separación del borde inferior (deja pasar la barra)
const ALPHA: u8 = 235; // casi opaco: bien visible, con un pelín de transparencia

/// Handle para DETENER el indicador. El indicador sólo debe verse mientras el
/// monitoreo está encendido; al soltar este handle (o llamar `detener`) la ventana
/// se cierra y su hilo termina. Es `Send` (guarda el HWND como entero).
pub struct IndicadorHandle {
    hwnd: isize, // HWND como entero (0 = no se pudo crear la ventana)
    hilo: Option<JoinHandle<()>>,
    permitido_pausa: bool, // eco de `permitido` al crear (para detectar cambios en monitoreo.rs)
}

impl IndicadorHandle {
    /// Cierra el indicador y espera a que su hilo termine. Idempotente y seguro
    /// de llamar desde cualquier hilo EXCEPTO el propio del indicador.
    pub fn detener(mut self) {
        self.parar();
    }

    /// Pide repintar el banner (ej. el agente auto-reanudó por tope de tiempo
    /// privado y el estado ya cambió por fuera de un clic). PostMessageW es
    /// thread-safe: se puede llamar desde el hilo del agente.
    pub fn repintar(&self) {
        if self.hwnd != 0 {
            unsafe {
                PostMessageW(self.hwnd as HWND, WM_REPINTAR, 0, 0);
            }
        }
    }

    /// Con qué `permitido` se creó este indicador (para que el llamador detecte
    /// si la política cambió y necesita recrearlo).
    pub fn permitido_pausa(&self) -> bool {
        self.permitido_pausa
    }

    fn parar(&mut self) {
        if let Some(hilo) = self.hilo.take() {
            if self.hwnd != 0 {
                // PostMessageW es thread-safe: encola WM_CLOSE en el hilo dueño de la
                // ventana. Ese hilo hace DestroyWindow -> WM_DESTROY -> PostQuitMessage,
                // GetMessageW devuelve 0 y el loop termina limpio.
                unsafe {
                    PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
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
/// Nunca hace panic: si la ventana no se puede crear, devuelve un handle "vacío"
/// (hwnd = 0) y lo registra en el log; el resto del monitoreo puede seguir.
pub fn iniciar_indicador(privado: Arc<AtomicBool>, permitido: bool) -> IndicadorHandle {
    let (tx, rx) = std::sync::mpsc::channel::<isize>();

    let hilo = std::thread::spawn(move || unsafe {
        correr_ventana(tx, EstadoIndicador { privado, permitido });
    });
    let permitido_pausa = permitido;

    // Esperamos (con tope) a que el hilo cree la ventana y nos devuelva su HWND.
    let hwnd = rx.recv_timeout(Duration::from_secs(5)).unwrap_or(0);
    if hwnd == 0 {
        log::warn!("[indicador] no se pudo crear la ventana del indicador visible");
    } else {
        log::info!("[indicador] indicador visible de monitoreo ACTIVO");
    }

    IndicadorHandle {
        hwnd,
        hilo: Some(hilo),
        permitido_pausa,
    }
}

/// Cuerpo del hilo del indicador: registra la clase (una vez), crea la ventana,
/// avisa el HWND por el canal y corre el message loop hasta WM_QUIT.
unsafe fn correr_ventana(tx: Sender<isize>, estado: EstadoIndicador) {
    let hinst = GetModuleHandleW(std::ptr::null()) as HINSTANCE;
    registrar_clase(hinst);

    // El estado vive en el hilo del indicador (aquí mismo), leído/escrito por
    // WM_PAINT y WM_LBUTTONUP.
    ESTADO.with(|c| *c.borrow_mut() = Some(estado));

    let clase = a_wide(CLASE);
    let titulo = a_wide("Dessau");
    let (x, y, w, h) = calcular_posicion();

    // Sin WS_EX_TRANSPARENT: el banner debe recibir clics (botón "Pausar
    // privado"). Sigue sin robar foco (WS_EX_NOACTIVATE) ni aparecer en la
    // barra de tareas (WS_EX_TOOLWINDOW).
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
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

    // Casi opaco pero un poco translúcido y CLICK-THROUGH (WS_EX_TRANSPARENT): nunca
    // tapa ni bloquea lo que hay debajo.
    SetLayeredWindowAttributes(hwnd, 0, ALPHA, LWA_ALPHA);
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    UpdateWindow(hwnd);
    // Reafirmar topmost cada 2 s por si otra ventana "always on top" se pone encima.
    SetTimer(hwnd, TIMER_TOPMOST, 2000, None);

    // Publicamos el HWND para que iniciar_indicador() pueda pararlo luego.
    let _ = tx.send(hwnd as isize);

    // Message loop: DEBE correr en este hilo (el que creó la ventana).
    let mut msg: MSG = std::mem::zeroed();
    // GetMessageW: >0 hay mensaje, 0 es WM_QUIT (salir), -1 es error (salir también).
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
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
/// (se pasa por puntero en WNDCLASSEXW), pero sí la ABI del sistema.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            pintar(hwnd);
            0
        }
        WM_LBUTTONUP => {
            // Clic del trabajador: alterna tiempo privado (solo si TI lo permite).
            ESTADO.with(|c| {
                if let Some(e) = c.borrow().as_ref() {
                    if e.permitido {
                        let nuevo = !e.privado.load(Ordering::SeqCst);
                        e.privado.store(nuevo, Ordering::SeqCst);
                        log::info!("[indicador] tiempo privado -> {nuevo} (clic del trabajador)");
                    }
                }
            });
            InvalidateRect(hwnd, std::ptr::null(), TRUE);
            0
        }
        WM_REPINTAR => {
            // El agente pide repintar (ej. auto-reanudó por tope de tiempo privado).
            InvalidateRect(hwnd, std::ptr::null(), TRUE);
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
            let (x, y, w, h) = calcular_posicion();
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
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

/// Pinta el banner: fondo oscuro + texto claro + el "●" en verde. Crea y libera
/// sus objetos GDI en cada WM_PAINT (el banner es estático, así que WM_PAINT es
/// raro); así no hay estado por-ventana que gestionar ni fugas de GDI.
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

    // Fondo oscuro (gris azulado).
    let brush: HBRUSH = CreateSolidBrush(color(24, 28, 36));
    if !brush.is_null() {
        FillRect(hdc, &rc, brush);
        DeleteObject(brush as HGDIOBJ);
    }

    // Fuente Segoe UI semibold ~13.5pt (altura negativa = alto de carácter).
    let face = a_wide("Segoe UI");
    let font: HFONT = CreateFontW(
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
    );
    let old: HGDIOBJ = if font.is_null() {
        std::ptr::null_mut()
    } else {
        SelectObject(hdc, font as HGDIOBJ)
    };

    SetBkMode(hdc, TRANSPARENT as i32);

    let mut rt = rc;
    rt.left += 14; // pequeño padding izquierdo
    let fmt = DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX | DT_NOCLIP;

    // Estado actual: leído del tiempo-privado compartido con el agente.
    let (privado, permitido) = ESTADO.with(|c| {
        c.borrow()
            .as_ref()
            .map(|e| (e.privado.load(Ordering::SeqCst), e.permitido))
            .unwrap_or((false, false))
    });

    let (texto, color_punto) = if privado {
        ("\u{23F8} Tiempo privado — clic para reanudar", color(230, 176, 46)) // ámbar
    } else if permitido {
        ("\u{25CF} Monitoreo activo — Dessau (clic = pausa privada)", color(46, 204, 113))
    } else {
        (TEXTO, color(46, 204, 113))
    };

    // 1) Texto completo en blanco.
    let full = a_wide(texto);
    SetTextColor(hdc, color(238, 238, 238));
    DrawTextW(hdc, full.as_ptr(), -1, &mut rt, fmt);

    // 2) El símbolo inicial ("●" o "⏸") repintado ENCIMA en su color, misma
    //    alineación izquierda: cae justo sobre el símbolo blanco de abajo.
    let simbolo: Vec<u16> = texto.chars().take(1).collect::<String>().encode_utf16().chain(std::iter::once(0)).collect();
    SetTextColor(hdc, color_punto);
    DrawTextW(hdc, simbolo.as_ptr(), -1, &mut rt, fmt);

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

/// Esquina inferior derecha del monitor primario. GetSystemMetrics da píxeles del
/// monitor primario en el contexto DPI del proceso (ver nota de DPI en el reporte).
unsafe fn calcular_posicion() -> (i32, i32, i32, i32) {
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let x = (sw - ANCHO - MARGEN).max(0);
    let y = (sh - ALTO - GAP_TASKBAR).max(0);
    (x, y, ANCHO, ALTO)
}

/// COLORREF (0x00BBGGRR) a partir de componentes RGB, sin depender de la macro RGB.
fn color(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// &str -> UTF-16 terminado en NUL (para las APIs *W de Win32).
fn a_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
