// =============================================================================
//  Transparencia del monitoreo (Ley 29733) — consentimiento + indicador visible
// -----------------------------------------------------------------------------
//  ⚖️ Este módulo es lo que hace LEGÍTIMO el monitoreo de productividad: el
//  trabajador está INFORMADO y lo VE. Sin esto, NO se mide ni se captura nada.
//    - `asegurar_consentimiento`: muestra el aviso (aviso_texto que devuelve el
//      servidor) y exige aceptación ANTES de cualquier medición/captura. Se
//      re-pide si cambia aviso_version. La aceptación se guarda por-usuario.
//    - `Indicador`: una señal visible y permanente de que el monitoreo está
//      activo mientras corre (el trabajador siempre sabe que está encendido).
//
//  NO hay evasión de antivirus ni ocultamiento: al contrario, esto se MUESTRA.
//  Corre en el proceso --tray (sesión del usuario). Reusa winapi (ya dep).
// =============================================================================
#![cfg(windows)]

use hbb_common::log;
use std::path::PathBuf;

/// Directorio de estado POR USUARIO (escribible por el usuario que corre --tray):
/// %APPDATA%\Dessau\monitoreo. Acá va el consentimiento y el estado del agente,
/// NO en %ProgramData% (que es de solo-lectura para el usuario).
pub fn dir_usuario() -> Option<PathBuf> {
    let ad = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(ad).join("Dessau").join("monitoreo"))
}

fn ruta_consentimiento() -> Option<PathBuf> {
    Some(dir_usuario()?.join("consentimiento"))
}

/// Versión de aviso ya aceptada por este usuario (0 si nunca aceptó).
fn version_aceptada() -> i64 {
    ruta_consentimiento()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

fn guardar_aceptacion(version: i64) {
    if let Some(dir) = dir_usuario() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("consentimiento"), version.to_string());
    }
}

fn a_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Muestra el aviso al usuario y exige que lo acepte ANTES de monitorear. Si ya
/// aceptó esta versión (o una mayor), no molesta. Devuelve true sólo si hay
/// consentimiento vigente. Ley 29733: informado + consentido + puede rechazar.
pub fn asegurar_consentimiento(aviso_texto: &str, aviso_version: i64) -> bool {
    if version_aceptada() >= aviso_version {
        return true;
    }
    use winapi::um::winuser::{
        MessageBoxW, IDOK, MB_ICONINFORMATION, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let cuerpo = if aviso_texto.trim().is_empty() {
        "Este equipo tiene activo el monitoreo de productividad de Dessau S&Z \
         (tiempo de uso y capturas de pantalla). Al aceptar, confirmás que fuiste \
         informado/a. Podés consultar la política con el área de sistemas."
            .to_string()
    } else {
        aviso_texto.to_string()
    };
    let titulo = "Monitoreo de productividad — Dessau S&Z";
    let wc = a_utf16(&cuerpo);
    let wt = a_utf16(titulo);
    let r = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wc.as_ptr(),
            wt.as_ptr(),
            MB_OKCANCEL | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
        )
    };
    if r == IDOK {
        guardar_aceptacion(aviso_version);
        log::info!("[monitoreo] consentimiento aceptado (aviso v{aviso_version})");
        true
    } else {
        log::info!("[monitoreo] consentimiento RECHAZADO — no se monitorea");
        false
    }
}
