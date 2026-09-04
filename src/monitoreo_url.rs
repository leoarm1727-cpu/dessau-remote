// =============================================================================
//  URL de la pestaña activa del navegador (para clasificar productividad)
// -----------------------------------------------------------------------------
//  ⚖️ Parte del monitoreo TRANSPARENTE y CONSENTIDO (Ley 29733): solo se lee
//  cuando hay seguimiento activo + consentimiento vigente + `capturar_urls` en la
//  config, y el edge ANULA la URL en intervalos privados. No hay navegación
//  encubierta: es lo que distingue "chrome + SEACE" (trabajo) de "chrome + ocio".
//
//  Cómo: UI Automation (crate `windows`). Se toma la ventana en primer plano y, si
//  es un navegador Chromium (Edge/Chrome/Brave), se busca el control de la barra de
//  direcciones (la "omnibox", un control Edit) recorriendo el árbol con el
//  ControlViewWalker y se lee su valor con el ValuePattern. Se recorre el árbol
//  (en vez de FindFirst con condición) para NO tener que construir un VARIANT, que
//  es la parte más frágil/version-dependiente de la API.
//
//  Firefox NO usa UI Automation de la misma forma (su barra no expone ValuePattern
//  fiable); se deja como no soportado (devuelve None) — mejor sin dato que un dato
//  equivocado. El .exe basta para clasificar "navegador"; la URL es un extra.
//
//  ⚠️ El perfil release usa `panic = 'abort'`: NADA de unwrap()/expect() acá.
//  COM se inicializa UNA vez por hilo (thread_local) y el objeto IUIAutomation se
//  cachea; el agente llama a esto repetidamente desde su hilo.
// =============================================================================
#![cfg(windows)]

use hbb_common::log;
use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    UIA_EditControlTypeId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// Navegadores Chromium cuya omnibox sí podemos leer por UI Automation.
fn es_navegador_soportado(exe: &str) -> bool {
    matches!(exe, "msedge.exe" | "chrome.exe" | "brave.exe")
}

thread_local! {
    // IUIAutomation cacheado por hilo. Se crea perezosamente en el primer uso.
    static AUTOMATION: RefCell<Option<IUIAutomation>> = RefCell::new(None);
}

/// Obtiene (o crea, una vez por hilo) el objeto IUIAutomation. Inicializa COM en
/// este hilo. Devuelve None si algo falla (no rompe el monitoreo).
fn con_automation<R>(f: impl FnOnce(&IUIAutomation) -> Option<R>) -> Option<R> {
    AUTOMATION.with(|celda| {
        let mut slot = celda.borrow_mut();
        if slot.is_none() {
            unsafe {
                // COINIT_APARTMENTTHREADED: UI Automation es STA-friendly. Si el hilo
                // ya tenía COM en otro modo, CoInitializeEx devuelve RPC_E_CHANGED_MODE;
                // lo toleramos (COM ya está utilizable) y seguimos.
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                match CoCreateInstance::<_, IUIAutomation>(
                    &CUIAutomation,
                    None,
                    CLSCTX_INPROC_SERVER,
                ) {
                    Ok(a) => *slot = Some(a),
                    Err(e) => {
                        log::warn!("[monitoreo] UIA no disponible: {e}");
                        return None;
                    }
                }
            }
        }
        slot.as_ref().and_then(f)
    })
}

/// Valor del ValuePattern de un elemento (la URL de la omnibox), si lo tiene y no
/// está vacío.
unsafe fn valor_de(el: &IUIAutomationElement) -> Option<String> {
    let vp = el
        .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        .ok()?;
    let bstr = vp.CurrentValue().ok()?;
    let s = bstr.to_string();
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Busca, en profundidad y acotado, el primer Edit con valor (la barra de
/// direcciones). Acotado por nodos y profundidad: la omnibox está cerca de la
/// barra superior, y así nunca recorremos un árbol gigante ni nos colgamos.
unsafe fn buscar_url(
    automation: &IUIAutomation,
    el: &IUIAutomationElement,
    profundidad: u32,
    visitados: &mut u32,
) -> Option<String> {
    const MAX_NODOS: u32 = 400;
    const MAX_PROF: u32 = 12;
    if *visitados >= MAX_NODOS || profundidad >= MAX_PROF {
        return None;
    }
    *visitados += 1;

    // ¿Este nodo es la barra de direcciones (Edit con valor tipo URL)?
    if let Ok(ct) = el.CurrentControlType() {
        if ct == UIA_EditControlTypeId {
            if let Some(v) = valor_de(el) {
                if parece_url(&v) {
                    return Some(v);
                }
            }
        }
    }

    // Recorrer hijos con el ControlViewWalker.
    let walker = automation.ControlViewWalker().ok()?;
    let mut hijo = walker.GetFirstChildElement(el).ok();
    while let Some(h) = hijo {
        if let Some(u) = buscar_url(automation, &h, profundidad + 1, visitados) {
            return Some(u);
        }
        if *visitados >= MAX_NODOS {
            break;
        }
        hijo = walker.GetNextSiblingElement(&h).ok();
    }
    None
}

/// Heurística: ¿el texto parece una URL/host y no lo que el usuario está tecleando?
/// La omnibox muestra la URL cuando no está enfocada; si el usuario está escribiendo
/// una búsqueda, suele no tener punto ni parecer host. Filtramos lo obvio.
fn parece_url(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 || s.len() > 2048 {
        return false;
    }
    if s.contains(' ') && !s.starts_with("http") {
        return false; // "cómo hacer X" (búsqueda), no una URL
    }
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("about:")
        || s.starts_with("edge://")
        || s.starts_with("chrome://")
        || s.contains('.') // "github.com/...", "docs.google.com"
}

/// Handle de la ventana en primer plano (o None si no hay).
pub fn hwnd_primer_plano() -> Option<HWND> {
    let h = unsafe { GetForegroundWindow() };
    if h.0.is_null() {
        None
    } else {
        Some(h)
    }
}

/// Devuelve la URL de la pestaña activa del navegador en primer plano, o None si
/// no es un navegador soportado / no se pudo leer. `exe` en minúsculas (ej.
/// "msedge.exe"). Nunca hace panic.
pub fn url_de_navegador(exe: &str) -> Option<String> {
    if !es_navegador_soportado(exe) {
        return None;
    }
    let hwnd = hwnd_primer_plano()?;
    con_automation(|automation| unsafe {
        let raiz = automation.ElementFromHandle(hwnd).ok()?;
        let mut visitados = 0u32;
        buscar_url(automation, &raiz, 0, &mut visitados)
    })
}
