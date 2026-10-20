

#![cfg(windows)]

use hbb_common::log;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

pub const VERSION_INSTALADOR: &str = match option_env!("DESSAU_INSTALADOR_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

const INTERVALO_BASE_SEG: u64 = 6 * 3600;
const ESPERA_INICIAL_SEG: u64 = 120;

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

fn url_version() -> Option<String> {
    let kv = dir_config().map(|d| leer_kv(&d.join("config")))?;
    let base = kv.get("DESSAU_MONITOREO_URL")?.clone();
    Some(base.replace("monitoreo-actividad-device", "monitoreo-version-agente"))
}

fn secreto_propio(dir: &PathBuf) -> Option<String> {
    // Camino normal: secreto DERIVADO del equipo (estable entre reinstalaciones), con una
    // sal distinta a la del canal de actividad para que ambos secretos no coincidan.
    if let Some(s) = crate::monitoreo::secreto_estable("dessau-monitoreo-update-v1") {
        return Some(s);
    }
    // Respaldo (MachineGuid ilegible): el UUID en archivo de siempre.
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
        return;
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

    let _ = std::fs::remove_file(&archivo);
}

pub fn ejecutar_como_servicio() {
    log::info!("[actualizacion] hot-update silencioso iniciado (versión actual: {VERSION_INSTALADOR})");
    std::thread::sleep(Duration::from_secs(ESPERA_INICIAL_SEG));

    let Some(dir) = dir_config() else {
        log::warn!("[actualizacion] sin %ProgramData% — inerte");
        return;
    };
    let Ok(cliente) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
    else {
        log::error!("[actualizacion] no se pudo crear el cliente HTTP");
        return;
    };

    loop {
        intentar_actualizar(&cliente, &dir);

        let jitter = (uuid::Uuid::new_v4().as_u128() % 3600) as i64 - 1800;
        let espera = (INTERVALO_BASE_SEG as i64 + jitter).max(3600) as u64;
        std::thread::sleep(Duration::from_secs(espera));
    }
}
