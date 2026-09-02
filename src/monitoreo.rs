// =============================================================================
//  Agente de monitoreo Dessau — fusionado en el cliente RustDesk
// -----------------------------------------------------------------------------
//  ⚠️ AVISO AGPL-3.0: este archivo es una MODIFICACIÓN al cliente RustDesk
//  (AGPL-3.0). Al distribuir este fork a las PCs, la empresa queda obligada por
//  la cláusula 13 de la AGPL a ofrecer el código fuente COMPLETO de esta versión
//  modificada a todos los usuarios que interactúen con ella por red. El código de
//  este agente (qué reporta) queda por tanto PÚBLICO. NO poner aquí secretos,
//  claves ni endpoints: van por configuración externa (archivo con ACL o
//  variables de entorno), NUNCA horneados en el binario. Ver LEEME-FORK.md.
//
//  Diseño: hilo independiente lanzado desde core_main.rs en el proceso `--service`,
//  que Windows autoarranca en CADA arranque (sc ... start=auto), ANTES del login, y
//  corre como LocalSystem. Por eso puede leer el config protegido (ACL sólo
//  Admin+SYSTEM) sin exponer el secreto a los usuarios, y funciona en PCs de
//  usuario estándar. NO se comunica con el resto de RustDesk: es lógica autónoma.
//
//  ALCANCE (Fase 1) = REGISTRO del equipo. El agente:
//    1. DA DE ALTA el equipo por hostname + IP (sin registrar usuario): el backend
//       detecta y audita si la IP o el nombre del equipo cambian.
//    2. Late periódicamente (mantiene "visto por última vez") y recibe del servidor
//       el intervalo de latido.
//  La medición de ACTIVIDAD (apps/ocio/foreground) es FASE 2: necesita la sesión
//  interactiva del usuario (GetForegroundWindow/GetLastInputInfo NO ven el
//  escritorio desde el servicio en la sesión 0). Irá en el tray, entregando las
//  muestras al servicio por IPC para que el secreto nunca salga del contexto SYSTEM.
// =============================================================================

#![cfg(windows)]

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

// `log` NO es dependencia directa del crate; se usa vía el re-export de hbb_common
// (igual que core_main.rs). Sin este `use`, las macros log::* no resuelven (E0433).
use hbb_common::log;

/// Versión del AGENTE (distinta de la versión del cliente RustDesk): así el panel
/// distingue iteraciones del agente de las del cliente. Bumpear al cambiar el agente.
const AGENTE_VERSION: &str = concat!("dessau-monitoreo/", env!("CARGO_PKG_VERSION"));

// ── Configuración (URL + secreto) ────────────────────────────────────────────
//  Orden de lectura: 1) archivo %ProgramData%\Dessau\monitoreo\config (ACL sólo
//  Administrators+SYSTEM, lo escribe el instalador), 2) variables de entorno.
//  Como el agente corre como SYSTEM (servicio), lee ese archivo sin problema y el
//  secreto queda ilegible para los usuarios estándar de la PC.
struct Config {
    url: String,
    secret: String,
    apikey: Option<String>, // opcional: si la edge exige apikey/Authorization
    envio_seg_semilla: u64,  // semilla; el servidor puede reemplazarla
}

/// Directorio de estado/config del agente: %ProgramData%\Dessau\monitoreo
fn dir_datos() -> Option<PathBuf> {
    let pd = std::env::var("ProgramData").ok()?;
    Some(PathBuf::from(pd).join("Dessau").join("monitoreo"))
}

/// Lee un archivo KEY=VALUE (estilo .env, sin comillas). Devuelve un mapa simple.
fn leer_archivo_kv(ruta: &PathBuf) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    if let Ok(txt) = std::fs::read_to_string(ruta) {
        for linea in txt.lines() {
            let l = linea.trim();
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

impl Config {
    fn cargar() -> Option<Self> {
        // 1) archivo de config (fuente preferida)
        let kv = dir_datos()
            .map(|d| leer_archivo_kv(&d.join("config")))
            .unwrap_or_default();
        // 2) fallback a entorno para cada clave
        let get = |k: &str| -> Option<String> {
            kv.get(k)
                .cloned()
                .or_else(|| std::env::var(k).ok())
                .filter(|s| !s.is_empty())
        };

        let url = get("DESSAU_MONITOREO_URL")?; // sin URL, el agente queda inerte
        let secret = get("DESSAU_MONITOREO_SECRET").unwrap_or_default();
        let apikey = get("DESSAU_MONITOREO_APIKEY");
        let envio_seg_semilla = get("DESSAU_MONITOREO_INTERVALO")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n >= 5)
            .unwrap_or(60);
        Some(Config { url, secret, apikey, envio_seg_semilla })
    }
}

/// Política vigente. En Fase 1 sólo interesa cada cuánto latir; el servidor manda.
struct Politica {
    envio_seg: u64,
}

impl Politica {
    fn semilla(envio_seg: u64) -> Self {
        Politica { envio_seg }
    }

    /// Actualiza desde el objeto `config` que devuelve la edge (defensivo).
    fn aplicar(&mut self, c: &serde_json::Value) {
        if let Some(n) = c.get("envio_seg").and_then(|v| v.as_u64()) {
            self.envio_seg = n.clamp(15, 3600);
        }
        // FASE 2: la edge también devuelve seguimiento_actividad, muestreo_seg,
        // idle_seg, capturar_titulos/urls, aviso_texto/aviso_version y tiempo_privado_*.
        // El REGISTRO no los necesita. La medición de actividad (tray) los consumirá,
        // e implementará el indicador/aviso visible de Ley 29733 que hoy queda cubierto
        // por el aviso a los trabajadores fuera de banda (obligatorio antes de encender).
    }
}

fn hostname() -> String {
    hbb_common::whoami::fallible::hostname().unwrap_or_else(|_| "desconocido".into())
}

/// Descripción del SO desde el registro (sin llamadas unsafe): "Windows 11 Pro 23H2 (build 22631)".
fn so_windows() -> String {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let hk = RegKey::predef(HKEY_LOCAL_MACHINE);
    match hk.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
        Ok(cv) => {
            let nombre: String = cv.get_value("ProductName").unwrap_or_else(|_| "Windows".into());
            let display: String = cv.get_value("DisplayVersion").unwrap_or_default();
            let build: String = cv.get_value("CurrentBuildNumber").unwrap_or_default();
            let mut s = nombre;
            if !display.is_empty() {
                s.push(' ');
                s.push_str(&display);
            }
            if !build.is_empty() {
                s.push_str(&format!(" (build {build})"));
            }
            s
        }
        Err(_) => "Windows".into(),
    }
}

/// IP LAN de origen HACIA el servidor de reporte. Truco de socket UDP "connect"
/// (no envía tráfico): el SO elige la interfaz que usaría para alcanzar ese host,
/// que en una PC multi-homed (red 0.x oficina vs 10.x aislada) es justo la correcta.
fn ip_local_hacia(endpoint: &str) -> Option<String> {
    use std::net::{ToSocketAddrs, UdpSocket};
    let host = url::Url::parse(endpoint).ok()?.host_str()?.to_string();
    // puerto tentativo según el esquema; sólo se usa para elegir la ruta.
    let puerto = if endpoint.starts_with("http://") { 80 } else { 443 };
    let destino = format!("{host}:{puerto}");
    let addr = destino.to_socket_addrs().ok()?.next()?;
    let sock = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.connect(addr).ok()?; // no envía paquetes; sólo fija la ruta de salida
    Some(sock.local_addr().ok()?.ip().to_string())
}

/// Detección de cambio de IP/hostname en un archivo de estado local. Guarda el
/// último par conocido; si cambia, devuelve true y reescribe (atómico: .tmp + rename).
/// El backend es el detector AUTORITATIVO (trigger sobre monitoreo_dispositivos);
/// este flag es informativo y a prueba de futuro.
fn detectar_cambio_red(ip: Option<&str>, host: &str) -> bool {
    let actual = format!("{}|{}", ip.unwrap_or(""), host);
    let dir = match dir_datos() {
        Some(d) => d,
        None => return false,
    };
    let ruta = dir.join("estado");
    let previo = std::fs::read_to_string(&ruta).ok();
    let cambio = match &previo {
        Some(p) => p.trim() != actual, // hay valor previo y difiere
        None => false,                 // primer arranque: no es un "cambio"
    };
    // Escribir solo si cambió o si aún no había estado (primer arranque): evita
    // reescribir el archivo en cada latido. Escritura atómica (.tmp + rename).
    if cambio || previo.is_none() {
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join("estado.tmp");
        if let Ok(mut f) = std::fs::File::create(&tmp) {
            if f.write_all(actual.as_bytes()).is_ok() {
                let _ = std::fs::rename(&tmp, &ruta);
            }
        }
    }
    cambio
}

/// Un latido de REGISTRO: reporta la identidad del equipo (sin muestras de
/// actividad, que son Fase 2) y devuelve la config vigente del servidor.
fn latido(
    cliente: &reqwest::blocking::Client,
    cfg: &Config,
    host: &str,
    ip: Option<&str>,
    so: &str,
    cambio_red: bool,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "dispositivo": host,
        "hostname": host,
        "ip": ip,
        "so": so,
        "agenteVersion": AGENTE_VERSION,
        "cambio_red": cambio_red,
        "muestras": [],   // Fase 1: registro puro; la actividad es Fase 2 (tray).
    });
    let mut req = cliente
        .post(&cfg.url)
        .header("x-device-secret", &cfg.secret)
        .json(&payload);
    // Si la edge exige apikey/JWT (verify_jwt=true), enviarla; si no, se ignora.
    if let Some(k) = &cfg.apikey {
        req = req.header("apikey", k).header("Authorization", format!("Bearer {k}"));
    }
    let resp = req.send().map_err(|e| format!("POST falló: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().map_err(|e| format!("respuesta no-JSON: {e}"))?;
    Ok(body.get("config").cloned().unwrap_or(serde_json::Value::Null))
}

/// Bucle del agente. Se ejecuta en su propio hilo (SYSTEM, en el servicio); nunca
/// retorna mientras el proceso viva. Sin config (sin URL) sale de inmediato (inerte).
///
/// ⚠️ El perfil release del fork usa `panic = 'abort'`: un panic acá tumbaría el
/// servicio. Todas las rutas usan `?`/unwrap_or/opciones — NO introducir unwrap ni
/// indexado sin guarda en este módulo.
pub fn ejecutar() {
    let cfg = match Config::cargar() {
        Some(c) => c,
        None => {
            log::info!("[monitoreo] sin DESSAU_MONITOREO_URL — agente inerte");
            return;
        }
    };
    log::info!("[monitoreo] agente {} iniciado (registro), reporta a {}", AGENTE_VERSION, cfg.url);

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

    let host = hostname();
    let so = so_windows();
    let mut pol = Politica::semilla(cfg.envio_seg_semilla);

    // Latido inmediato: da de alta el equipo (hostname + IP) apenas arranca la PC,
    // antes del login; luego uno cada envio_seg. El backend detecta cambios de IP/nombre.
    loop {
        let ip = ip_local_hacia(&cfg.url);
        let cambio = detectar_cambio_red(ip.as_deref(), &host);
        match latido(&cliente, &cfg, &host, ip.as_deref(), &so, cambio) {
            Ok(c) => pol.aplicar(&c),
            Err(e) => log::warn!("[monitoreo] latido falló: {e}"),
        }
        std::thread::sleep(Duration::from_secs(pol.envio_seg));
    }
}
