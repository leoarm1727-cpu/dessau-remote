// =============================================================================
//  Agente de monitoreo de productividad — fusionado en el cliente RustDesk
// -----------------------------------------------------------------------------
//  ⚠️ AVISO AGPL-3.0: este archivo es una MODIFICACIÓN al cliente RustDesk
//  (AGPL-3.0). Al distribuir este fork a las PCs, la empresa queda obligada por
//  la cláusula 13 de la AGPL a ofrecer el código fuente COMPLETO de esta versión
//  modificada a todos los usuarios que interactúen con ella por red. El código de
//  este agente (cómo mide, qué reporta) queda por tanto PÚBLICO. NO poner aquí
//  secretos, claves ni endpoints: van por configuración externa (variables de
//  entorno / archivo de config fuera del binario). Ver memoria del proyecto.
//
//  Diseño: hilo independiente lanzado desde core_main.rs al arrancar el servicio.
//  Mide en Windows con Win32 (GetLastInputInfo, GetForegroundWindow) y reporta
//  por HTTP con reqwest (ya dependencia del proyecto). NO se comunica con el
//  resto de RustDesk: es lógica autónoma que coexiste en el mismo binario.
// =============================================================================

#![cfg(windows)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Configuración del agente, leída del entorno (NUNCA horneada en el código).
/// - DESSAU_MONITOREO_URL:    endpoint HTTP donde reportar (p.ej. http://192.168.0.10:3800/api/...).
/// - DESSAU_MONITOREO_SECRET: secreto de dispositivo para autenticar el reporte.
/// - DESSAU_MONITOREO_INTERVALO: segundos entre reportes (default 60).
/// Si falta la URL, el agente NO arranca (queda inerte, sin reportar).
struct Config {
    url: String,
    secret: String,
    intervalo: Duration,
}

impl Config {
    fn desde_entorno() -> Option<Self> {
        let url = std::env::var("DESSAU_MONITOREO_URL").ok().filter(|s| !s.is_empty())?;
        let secret = std::env::var("DESSAU_MONITOREO_SECRET").unwrap_or_default();
        let intervalo = std::env::var("DESSAU_MONITOREO_INTERVALO")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n >= 5)
            .unwrap_or(60);
        Some(Config { url, secret, intervalo: Duration::from_secs(intervalo) })
    }
}

/// Milisegundos que el usuario lleva inactivo (sin teclado ni mouse).
/// GetLastInputInfo devuelve el tick del último input; restamos del tick actual.
fn inactividad_ms() -> u64 {
    use winapi::um::sysinfoapi::GetTickCount;
    use winapi::um::winuser::{GetLastInputInfo, LASTINPUTINFO};
    unsafe {
        let mut lii = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut lii) == 0 {
            return 0;
        }
        let ahora = GetTickCount();
        ahora.wrapping_sub(lii.dwTime) as u64
    }
}

/// Título de la ventana en primer plano (app que el usuario está usando).
fn ventana_primer_plano() -> String {
    use winapi::um::winuser::{GetForegroundWindow, GetWindowTextW, GetWindowTextLengthW};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return String::new();
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf: Vec<u16> = vec![0u16; (len + 1) as usize];
        let leidos = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if leidos <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..leidos as usize])
    }
}

fn epoch_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn hostname() -> String {
    hbb_common::whoami::fallible::hostname().unwrap_or_else(|_| "desconocido".into())
}

/// Un latido: mide y reporta. Devuelve Ok si el POST fue aceptado.
fn latido(cfg: &Config, cliente: &reqwest::blocking::Client) -> Result<(), String> {
    let inactivo_ms = inactividad_ms();
    // Umbral de ocio: 60 s sin input = ocioso (misma convención que el agente PS).
    let ocioso = inactivo_ms >= 60_000;
    let payload = serde_json::json!({
        "dispositivo": hostname(),
        "ts": epoch_secs(),
        "inactivo_ms": inactivo_ms,
        "ocioso": ocioso,
        "ventana": ventana_primer_plano(),
        "origen": "rustdesk-fork",
    });
    let resp = cliente
        .post(&cfg.url)
        .header("x-device-secret", &cfg.secret)
        .json(&payload)
        .send()
        .map_err(|e| format!("POST falló: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", resp.status()))
    }
}

/// Bucle del agente. Se ejecuta en su propio hilo; nunca retorna mientras el
/// proceso viva. Si no hay config, sale de inmediato (agente inerte).
pub fn ejecutar() {
    let cfg = match Config::desde_entorno() {
        Some(c) => c,
        None => {
            log::info!("[monitoreo] sin DESSAU_MONITOREO_URL — agente inerte");
            return;
        }
    };
    log::info!("[monitoreo] agente iniciado, reporta a {} cada {:?}", cfg.url, cfg.intervalo);
    let cliente = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("[monitoreo] no se pudo crear el cliente HTTP: {e}");
            return;
        }
    };
    loop {
        if let Err(e) = latido(&cfg, &cliente) {
            log::warn!("[monitoreo] latido falló: {e}");
        }
        std::thread::sleep(cfg.intervalo);
    }
}
