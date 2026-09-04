// =============================================================================
//  Hot-update SILENCIOSO del agente Dessau — sin notificaciones
// -----------------------------------------------------------------------------
//  Corre en el proceso `--service` (SYSTEM, desde el boot), NUNCA en `--tray`.
//  Por qué: el instalador exige admin (PrivilegesRequired=admin). Si el chequeo
//  lo disparara el --tray (sesión del usuario, sin privilegios), instalar la
//  nueva versión mostraría el prompt de UAC — una notificación visible. El
//  servicio YA es SYSTEM, así que el instalador silencioso corre sin UAC y sin
//  ninguna ventana (session 0 no tiene escritorio interactivo de todos modos).
//
//  Reemplaza al updater STOCK de RustDesk (api.rustdesk.com, el proyecto
//  OFICIAL — desactivado en common.rs::check_software_update) por uno que
//  apunta a NUESTRAS builds, publicadas por el CI del fork vía la edge
//  `monitoreo-publicar-release` y servidas por `monitoreo-version-agente`.
//
//  Auth: TOFU por-dispositivo, identidad SEPARADA del secreto del --tray
//  (`update_secret_hash` en vez de `secret_hash`, misma fila por hostname).
//  El secreto vive en %ProgramData%\Dessau\monitoreo\update-secret
//  (SYSTEM-writable; ver la nota de confianza local en `secreto_propio`).
//
//  ⚠️ El perfil release usa `panic = 'abort'`: NADA de unwrap()/expect() en
//  rutas alcanzables. Loop propio en su hilo, no toca el runtime tokio del
//  servicio (rendezvous_mediator / IPC) para no arriesgar el acceso remoto.
// =============================================================================
#![cfg(windows)]

use hbb_common::log;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// Versión de ESTE build, para comparar contra la última release publicada.
/// La fija el CI en cada build (DESSAU_INSTALADOR_VERSION, ej. "1.5.0.147");
/// en un build local (sin esa env) cae a la versión de Cargo, sin auto-update
/// real (el CI nunca publica una release con ese formato) — comportamiento
/// seguro por defecto.
pub const VERSION_INSTALADOR: &str = match option_env!("DESSAU_INSTALADOR_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Cada cuánto chequear (con jitter, para no pegarle a la edge toda la flota
/// al mismo minuto si todos bootean juntos un lunes).
const INTERVALO_BASE_SEG: u64 = 6 * 3600; // 6 horas
const ESPERA_INICIAL_SEG: u64 = 120; // deja que el servicio termine de arrancar

fn dir_config() -> Option<PathBuf> {
    let pd = std::env::var("ProgramData").ok()?;
    Some(PathBuf::from(pd).join("Dessau").join("monitoreo"))
}

fn leer_kv(ruta: &PathBuf) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    if let Ok(txt) = std::fs::read_to_string(ruta) {
        for l in txt.lines() {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    m
}

/// URL de chequeo de versión, derivada de la MISMA config que usa --tray
/// (%ProgramData%\Dessau\monitoreo\config), reemplazando el nombre de la edge.
fn url_version() -> Option<String> {
    let kv = dir_config().map(|d| leer_kv(&d.join("config")))?;
    let base = kv.get("DESSAU_MONITOREO_URL")?.clone();
    Some(base.replace("monitoreo-actividad-device", "monitoreo-version-agente"))
}

/// Secreto propio del SERVICIO (identidad separada del --tray). Confianza
/// local: %ProgramData% es escribible por usuarios estándar en este diseño
/// (igual que el resto de %ProgramData%\Dessau\monitoreo), así que un usuario
/// con acceso físico a SU PROPIA PC podría re-disparar el TOFU de este archivo
/// — el peor caso es que su propia PC vuelva a registrar identidad de update;
/// no compromete a otras PCs ni da más que "consultar si hay versión nueva".
fn secreto_propio(dir: &PathBuf) -> Option<String> {
    let ruta = dir.join("update-secret");
    if let Ok(s) = std::fs::read_to_string(&ruta) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    let nuevo = uuid::Uuid::new_v4().simple().to_string();
    let _ = std::fs::create_dir_all(dir);
    if std::fs::write(&ruta, &nuevo).is_err() {
        return None;
    }
    Some(nuevo)
}

fn hostname() -> String {
    hbb_common::whoami::fallible::hostname().unwrap_or_else(|_| "desconocido".into())
}

fn sha256_archivo(ruta: &PathBuf) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(ruta).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex::encode(hasher.finalize()))
}

/// Un chequeo: consulta la edge, y si hay una release nueva la descarga,
/// verifica el sha256 y la instala en silencio. Nunca hace panic: cualquier
/// paso que falle solo loguea y se reintenta en el próximo ciclo.
fn intentar_actualizar(cliente: &reqwest::blocking::Client, dir: &PathBuf) {
    let Some(url) = url_version() else {
        log::debug!("[actualizacion] sin DESSAU_MONITOREO_URL — inerte");
        return;
    };
    let Some(secreto) = secreto_propio(dir) else {
        log::warn!("[actualizacion] no se pudo leer/crear el secreto propio");
        return;
    };
    let host = hostname();

    let payload = serde_json::json!({
        "dispositivo": host, "hostname": host, "versionActual": VERSION_INSTALADOR,
    });
    let resp = match cliente
        .post(&url)
        .header("x-device-secret", &secreto)
        .json(&payload)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            log::debug!("[actualizacion] chequeo falló: {e}");
            return;
        }
    };
    if !resp.status().is_success() {
        log::debug!("[actualizacion] chequeo HTTP {}", resp.status());
        return;
    }
    let body: serde_json::Value = match resp.json() {
        Ok(b) => b,
        Err(e) => {
            log::debug!("[actualizacion] respuesta no-JSON: {e}");
            return;
        }
    };
    if !body.get("hayActualizacion").and_then(|v| v.as_bool()).unwrap_or(false) {
        return; // ya estamos al día (o el equipo está deshabilitado)
    }
    let (Some(version), Some(sha_esperado), Some(descarga_url)) = (
        body.get("version").and_then(|v| v.as_str()),
        body.get("sha256").and_then(|v| v.as_str()),
        body.get("url").and_then(|v| v.as_str()),
    ) else {
        log::warn!("[actualizacion] respuesta incompleta del servidor");
        return;
    };
    log::info!("[actualizacion] nueva versión disponible: {version} (actual: {VERSION_INSTALADOR})");

    let dir_descargas = dir.join("actualizaciones");
    if std::fs::create_dir_all(&dir_descargas).is_err() {
        log::warn!("[actualizacion] no se pudo crear el directorio de descargas");
        return;
    }
    let archivo = dir_descargas.join(format!("Dessau-Setup-{version}.exe"));

    let mut resp = match cliente.get(descarga_url).send() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[actualizacion] descarga falló: {e}");
            return;
        }
    };
    if !resp.status().is_success() {
        log::warn!("[actualizacion] descarga HTTP {}", resp.status());
        return;
    }
    let tmp = dir_descargas.join(format!("Dessau-Setup-{version}.exe.tmp"));
    {
        let Ok(mut f) = std::fs::File::create(&tmp) else {
            log::warn!("[actualizacion] no se pudo crear el archivo temporal");
            return;
        };
        if std::io::copy(&mut resp, &mut f).is_err() {
            log::warn!("[actualizacion] no se pudo escribir la descarga");
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let _ = f.flush();
    }

    // Verificación de integridad ANTES de ejecutar nada.
    let sha_real = sha256_archivo(&tmp);
    if sha_real.as_deref() != Some(sha_esperado) {
        log::error!(
            "[actualizacion] sha256 NO coincide (esperado {sha_esperado}, obtenido {:?}) — se descarta",
            sha_real
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &archivo).is_err() {
        log::warn!("[actualizacion] no se pudo renombrar el instalador verificado");
        let _ = std::fs::remove_file(&tmp);
        return;
    }

    log::info!("[actualizacion] instalando {version} en silencio (SYSTEM, sin UAC ni ventanas)");
    // /VERYSILENT+/SUPPRESSMSGBOXES: cero UI. /CLOSEAPPLICATIONS+/RESTARTAPPLICATIONS:
    // cierra el rustdesk.exe en ejecución (--tray) para poder sobrescribir sus
    // archivos, y lo vuelve a lanzar al terminar (requiere CloseApplications=force
    // en el .iss). /NORESTART: nunca reiniciar Windows.
    let salida = std::process::Command::new(&archivo)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
            "/RESTARTAPPLICATIONS",
            "/NOICONS",
        ])
        .status();
    match salida {
        Ok(s) if s.success() => {
            log::info!("[actualizacion] {version} instalada correctamente");
        }
        Ok(s) => {
            log::error!("[actualizacion] el instalador terminó con código {:?}", s.code());
        }
        Err(e) => {
            log::error!("[actualizacion] no se pudo ejecutar el instalador: {e}");
        }
    }
    // Limpieza: ya sea que haya salido bien o mal, no dejar el .exe descargado.
    let _ = std::fs::remove_file(&archivo);
}

/// Bucle del chequeo de hot-update. Se llama UNA vez desde el arranque del
/// servicio (platform/windows.rs::run_service), en su propio hilo dedicado.
pub fn ejecutar_como_servicio() {
    log::info!("[actualizacion] hot-update silencioso iniciado (versión actual: {VERSION_INSTALADOR})");
    std::thread::sleep(Duration::from_secs(ESPERA_INICIAL_SEG));

    let Some(dir) = dir_config() else {
        log::warn!("[actualizacion] sin %ProgramData% — inerte");
        return;
    };
    let Ok(cliente) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120)) // el GET de descarga puede tardar (instalador ~21 MB)
        .build()
    else {
        log::error!("[actualizacion] no se pudo crear el cliente HTTP");
        return;
    };

    loop {
        intentar_actualizar(&cliente, &dir);
        // Jitter ±30 min para no sincronizar toda la flota en el mismo instante.
        let jitter = (uuid::Uuid::new_v4().as_u128() % 3600) as i64 - 1800;
        let espera = (INTERVALO_BASE_SEG as i64 + jitter).max(3600) as u64;
        std::thread::sleep(Duration::from_secs(espera));
    }
}
