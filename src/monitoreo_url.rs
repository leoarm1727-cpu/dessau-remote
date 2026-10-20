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

fn es_navegador_soportado(exe: &str) -> bool {
    matches!(exe, "msedge.exe" | "chrome.exe" | "brave.exe")
}

thread_local! {

    static AUTOMATION: RefCell<Option<IUIAutomation>> = RefCell::new(None);
}

fn con_automation<R>(f: impl FnOnce(&IUIAutomation) -> Option<R>) -> Option<R> {
    AUTOMATION.with(|celda| {
        let mut slot = celda.borrow_mut();
        if slot.is_none() {
            unsafe {

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

    if let Ok(ct) = el.CurrentControlType() {
        if ct == UIA_EditControlTypeId {
            if let Some(v) = valor_de(el) {
                if parece_url(&v) {
                    return Some(v);
                }
            }
        }
    }

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

fn parece_url(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 || s.len() > 2048 {
        return false;
    }
    if s.contains(' ') && !s.starts_with("http") {
        return false;
    }
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("about:")
        || s.starts_with("edge://")
        || s.starts_with("chrome://")
        || s.contains('.')
}

pub fn hwnd_primer_plano() -> Option<HWND> {
    let h = unsafe { GetForegroundWindow() };
    if h.0.is_null() {
        None
    } else {
        Some(h)
    }
}

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
