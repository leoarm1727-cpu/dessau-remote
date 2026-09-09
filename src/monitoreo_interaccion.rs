// =============================================================================
//  Contadores AGREGADOS de interacción (Reglamento RI-TI-001, cap. «Contadores
//  de interacción: validez y descargo, nunca umbral»)
// -----------------------------------------------------------------------------
//  ⚖️ ESTO NO ES UN KEYLOGGER, Y EL CÓDIGO TIENE QUE PODER AUDITARSE COMO TAL.
//
//  Lo ÚNICO que sale de este módulo son TRES NÚMEROS por intervalo:
//    • pulsaciones     — cuántas veces bajó una tecla (cualquiera).
//    • clics           — cuántas veces bajó un botón del mouse (cualquiera).
//    • seg_movimiento  — cuántos SEGUNDOS DISTINTOS hubo movimiento de mouse.
//
//  Lo que este módulo TIENE PROHIBIDO hacer, y no hace:
//    • Leer QUÉ tecla se pulsó. El campo `VKey` / `MakeCode` de RAWKEYBOARD
//      NUNCA se lee (solo `Flags`, para distinguir bajar de soltar). Si alguien
//      agrega una lectura de VKey a este archivo, está violando el Reglamento.
//    • Guardar contenido, texto, portapapeles ni selección: no se toca nada de eso.
//    • Guardar —ni siquiera leer— COORDENADAS del puntero. Con un mouse normal,
//      `lLastX`/`lLastY` son un DESPLAZAMIENTO y solo se comparan contra cero. Con
//      un dispositivo ABSOLUTO (RDP, VM, tableta, táctil) esos campos serían una
//      POSICIÓN, así que ahí NO se los mira: el movimiento se deduce de que el
//      reporte no traiga ningún botón (`usButtonFlags == 0`), que es lo que
//      distingue un movimiento de un clic quieto. Nunca se guarda una posición.
//      Esta rama subcuenta el caso «clic mientras se mueve», y esa dirección es
//      la que favorece al trabajador: menos movimiento contado = más fácil que se
//      dispare la compuerta que le descarta tiempo que no lo representa.
//    • Guardar el EVENTO individual o su marca de tiempo: no hay traza, solo
//      acumuladores. La cadencia por evento es rasgo biométrico conductual y por
//      eso está prohibida en el propio Reglamento.
//    • Distinguir QUÉ botón se pulsó: se cuenta el hecho, no el botón.
//
//  Cómo se mide: RAW INPUT (RegisterRawInputDevices + RIDEV_INPUTSINK) sobre una
//  ventana SOLO-MENSAJES (HWND_MESSAGE, invisible y sin escritorio) que vive en su
//  propio hilo con message loop —igual que monitoreo_indicador, porque las
//  ventanas Win32 entregan mensajes al hilo que las creó y el hilo del agente se
//  bloquea en HTTP—. No se usan hooks globales (WH_KEYBOARD_LL): un hook recibe
//  la tecla aunque no la mires, y acá ni siquiera queremos recibirla.
//
//  Ciclo de vida: se enciende SOLO mientras el monitoreo está activo y con
//  consentimiento vigente (igual que el indicador visible); al soltar el handle
//  se cierra la ventana y muere el hilo. Sin monitoreo no hay contadores.
//
//  ⚠️ El perfil release del fork usa `panic = 'abort'`: un panic acá tumbaría el
//  proceso --tray. Nada de unwrap()/expect() ni indexado sin guarda.
//  ⚠️ AGPL-3.0: es una modificación del cliente RustDesk. Sin secretos horneados.
// =============================================================================
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

/// Acumuladores globales. Son totales, no una traza: no hay forma de reconstruir
/// qué pasó ni cuándo. `tomar()` los lee y los pone en cero de una sola vez.
static PULSACIONES: AtomicU64 = AtomicU64::new(0);
static CLICS: AtomicU64 = AtomicU64::new(0);
static SEG_MOVIMIENTO: AtomicU64 = AtomicU64::new(0);
/// Último segundo (reloj monótono del sistema) ya contado como "con movimiento".
/// Evita contar mil veces el mismo segundo; no es una marca de tiempo guardada.
static ULTIMO_SEG_MOV: AtomicU64 = AtomicU64::new(u64::MAX);

/// Máscara de "bajó un botón" — cualquiera de ellos. No se guarda CUÁL.
const BOTON_ABAJO: u16 = RI_MOUSE_LEFT_BUTTON_DOWN
    | RI_MOUSE_RIGHT_BUTTON_DOWN
    | RI_MOUSE_MIDDLE_BUTTON_DOWN
    | RI_MOUSE_BUTTON_4_DOWN
    | RI_MOUSE_BUTTON_5_DOWN;

/// Tres totales de un intervalo.
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

/// Lee los acumuladores y los deja en CERO. Lo que se lleva el llamador es lo
/// ocurrido desde la lectura anterior: eso, y no otra cosa, es lo que se imputa
/// al intervalo que estaba abierto en ese lapso.
pub fn tomar() -> Contadores {
    Contadores {
        pulsaciones: PULSACIONES.swap(0, Ordering::Relaxed),
        clics: CLICS.swap(0, Ordering::Relaxed),
        seg_movimiento: SEG_MOVIMIENTO.swap(0, Ordering::Relaxed),
    }
}

/// Handle para DETENER el contador. Mismo patrón que `IndicadorHandle`: mientras
/// no haya monitoreo activo, este objeto no existe y no se cuenta nada.
pub struct ContadorHandle {
    /// HWND como entero, COMPARTIDO con el hilo (0 = todavía/nunca creado). Es un
    /// atómico y no una copia porque el hilo puede crear la ventana DESPUÉS de que
    /// venza el handshake: con una copia en cero, `parar()` no mandaría el WM_CLOSE
    /// y el `join()` colgaría el agente para siempre.
    hwnd: Arc<AtomicI64>,
    hilo: Option<JoinHandle<()>>,
}

impl ContadorHandle {
    /// ¿El contador está midiendo de verdad? Si es `false`, el agente NO debe
    /// reportar ceros: debe reportar «no hay dato» (null), porque cero significa
    /// «medí y no hubo interacción» y dispararía la compuerta contra el trabajador.
    pub fn activo(&self) -> bool {
        self.hwnd.load(Ordering::SeqCst) != 0
    }

    /// Cierra la ventana solo-mensajes, espera al hilo y descarta lo que quedara
    /// sin leer (al apagar el monitoreo no se arrastra nada al próximo encendido).
    pub fn detener(mut self) {
        self.parar();
    }

    fn parar(&mut self) {
        if let Some(hilo) = self.hilo.take() {
            // Se relee el HWND: si la ventana se creó tarde (handshake vencido),
            // acá ya está y el hilo recibe su WM_CLOSE. Si vale 0, el hilo terminó
            // solo por fallo de creación o de registro, y el join no bloquea.
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                unsafe {
                    PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }
            let _ = hilo.join();
        }
        let _ = tomar(); // se descarta: sin monitoreo no hay dato
    }
}

impl Drop for ContadorHandle {
    fn drop(&mut self) {
        self.parar();
    }
}

/// Enciende el contador en su propio hilo. Nunca hace panic —ni siquiera si el
/// sistema no puede crear el hilo: se usa `Builder::spawn`, que devuelve `Result`,
/// porque el release del fork es `panic = 'abort'` y un panic acá tumbaría el
/// proceso `--tray` entero (con él, el indicador visible de la Ley 29733)—. Si algo
/// falla devuelve un handle con `activo() == false`, y el agente debe entonces
/// reportar NULL, nunca ceros.
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

    // El handshake solo sirve para loguear a tiempo: la fuente de verdad del HWND
    // es el atómico, que el hilo escribe apenas crea la ventana.
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

/// Hilo del contador: clase + ventana solo-mensajes + raw input + message loop.
unsafe fn correr_ventana(tx: Sender<isize>, hwnd_compartido: Arc<AtomicI64>) {
    let hinst = GetModuleHandleW(std::ptr::null()) as HINSTANCE;
    registrar_clase(hinst);

    let clase = a_wide(CLASE);
    let titulo = a_wide("Dessau");
    // HWND_MESSAGE: ventana SOLO-MENSAJES. No se dibuja, no está en el escritorio,
    // no aparece en la barra de tareas y no puede recibir foco. Solo recibe WM_INPUT.
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
    // Publicado ANTES de registrar raw input: si el registro falla se vuelve a 0
    // abajo, y mientras tanto `parar()` ya sabe a qué ventana mandarle el WM_CLOSE.
    hwnd_compartido.store(hwnd as i64, Ordering::SeqCst);

    // RIDEV_INPUTSINK: recibimos el evento aunque la ventana no tenga el foco
    // (es una ventana invisible: nunca lo tiene). Usage page 0x01 (genérico),
    // usage 6 = teclado, usage 2 = mouse.
    let devs = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06, // teclado
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02, // mouse
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
    // GetMessageW devuelve 0 con WM_QUIT y -1 ante error: cualquiera de los dos
    // termina el loop (comparación con 1, como en monitoreo_indicador).
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    // La ventana ya no existe: nadie debe creer que el contador sigue midiendo.
    hwnd_compartido.store(0, Ordering::SeqCst);
}

unsafe fn registrar_clase(hinst: HINSTANCE) {
    let clase = a_wide(CLASE);
    let mut wc: WNDCLASSEXW = std::mem::zeroed();
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinst;
    wc.lpszClassName = clase.as_ptr();
    // Re-registrar la misma clase devuelve 0 y no es un error para nosotros
    // (puede pasar si se apaga y enciende el monitoreo en la misma corrida).
    RegisterClassExW(&wc);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            contar(lp);
            // El doc de Win32 pide pasar WM_INPUT al DefWindowProc para la limpieza.
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

/// Núcleo de la medición. Toda la lectura del evento está acá, y es corta a
/// propósito: del teclado solo se mira `Flags` (bajar/soltar); del mouse, solo
/// `usFlags` (¿el dispositivo reporta posición o desplazamiento?), `usButtonFlags`
/// (¿bajó algún botón? — no cuál) y, únicamente en el caso de desplazamiento, si
/// `lLastX/lLastY` son distintos de cero. NO se lee `VKey`, NO se lee `MakeCode`,
/// y NUNCA se lee ni se guarda una posición del puntero.
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
    // GetRawInputData devuelve (UINT)-1 ante error, y 0 si no copió nada.
    if leidos == 0 || leidos == u32::MAX {
        return;
    }

    match raw.header.dwType {
        RIM_TYPEKEYBOARD => {
            let kb = raw.data.keyboard();
            // Solo el flanco de BAJADA (RI_KEY_BREAK marca la subida). Incluye la
            // autorrepetición del teclado, y así se declara en el Reglamento.
            if (kb.Flags as u32 & RI_KEY_BREAK) == 0 {
                PULSACIONES.fetch_add(1, Ordering::Relaxed);
            }
        }
        RIM_TYPEMOUSE => {
            let m = raw.data.mouse();
            if (m.usButtonFlags & BOTON_ABAJO) != 0 {
                CLICS.fetch_add(1, Ordering::Relaxed);
            }
            // ¿Hubo movimiento? En el mouse NORMAL, lLastX/lLastY son un
            // DESPLAZAMIENTO y basta compararlos contra cero. En un dispositivo
            // ABSOLUTO (RDP, VM, tableta, táctil) esos campos son una POSICIÓN, y
            // mirarlos sería leer la coordenada del puntero, que el Reglamento
            // prohíbe RECOLECTAR (no solo guardar). Ahí se usa la única señal que
            // no es coordenada: un reporte sin ningún botón es un reporte de
            // movimiento; el clic quieto trae botón y no cuenta. Subcuenta el clic
            // hecho mientras se mueve, y esa dirección favorece al trabajador.
            let se_movio = if (m.usFlags & MOUSE_MOVE_ABSOLUTE) != 0 {
                m.usButtonFlags == 0
            } else {
                m.lLastX != 0 || m.lLastY != 0
            };
            if se_movio {
                let seg = (GetTickCount() as u64) / 1000;
                // Un segundo se cuenta UNA vez, por más eventos que lleguen.
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
