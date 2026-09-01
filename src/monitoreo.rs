// =============================================================================
//  Agente de monitoreo de productividad — fusionado en el cliente RustDesk
// -----------------------------------------------------------------------------
//  ⚠️ AVISO AGPL-3.0: este archivo es una MODIFICACIÓN al cliente RustDesk
//  (AGPL-3.0). Al distribuir este fork a las PCs, la empresa queda obligada por
//  la cláusula 13 de la AGPL a ofrecer el código fuente COMPLETO de esta versión
//  modificada a todos los usuarios que interactúen con ella por red. El código de
//  este agente (cómo mide, qué reporta) queda por tanto PÚBLICO. NO poner aquí
//  secretos, claves ni endpoints: van por configuración externa (archivo con ACL
//  o variables de entorno), NUNCA horneados en el binario. Ver LEEME-FORK.md.
//
//  Diseño: hilo independiente lanzado desde core_main.rs al arrancar el proceso
//  `--tray` (el que Windows autoarranca en CADA logon y corre en la sesión
//  interactiva del usuario). Mide en Windows con Win32 (GetLastInputInfo,
//  GetForegroundWindow) y reporta por HTTP con reqwest (ya dependencia). NO se
//  comunica con el resto de RustDesk: es lógica autónoma que coexiste en el
//  mismo binario.
//
//  Destino: la app Gestión Dessau (edge function `monitoreo-actividad-device`
//  de Supabase), NO Gauzy. El agente:
//    1. DA DE ALTA el equipo por hostname + IP (sin registrar usuario): el
//       backend detecta y audita si la IP o el nombre del equipo cambian.
//    2. OBEDECE la política que devuelve el servidor en cada latido (encender/
//       apagar seguimiento, cada cuánto muestrear, capturar títulos, etc.): la
//       política vive en el servidor, no en el agente (Ley 29733: TI manda).
//    3. Reporta la actividad (activo/ocioso) como intervalos, sólo si el
//       servidor lo tiene ENCENDIDO (arranca apagado).
// =============================================================================

#![cfg(windows)]

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Versión del AGENTE (distinta de la versión del cliente RustDesk): así el panel
/// distingue iteraciones del agente de las del cliente. Bumpear al cambiar el agente.
const AGENTE_VERSION: &str = concat!("dessau-monitoreo/", env!("CARGO_PKG_VERSION"));

// ── Configuración de arranque (URL + secreto) ────────────────────────────────
//  Orden de lectura: 1) archivo %ProgramData%\Dessau\monitoreo\config (ACL sólo
//  Administrators+SYSTEM, lo escribe el instalador), 2) variables de entorno.
//  El archivo es más robusto que el entorno para un proceso de arranque y permite
//  rotar el secreto/servidor sin depender de que el proceso herede setx /M.
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

/// Política vigente. Arranca CONSERVADORA (seguimiento apagado): el agente sólo
/// da de alta el equipo y consulta config hasta que TI encienda el seguimiento.
struct Politica {
    seguimiento: bool,
    equipo_habilitado: bool,
    muestreo_seg: u64,
    envio_seg: u64,
    idle_seg: u64,
    capturar_titulos: bool,
}

impl Politica {
    fn semilla(envio_seg: u64) -> Self {
        Politica {
            seguimiento: false,      // arranca APAGADO; el servidor manda
            equipo_habilitado: true,
            muestreo_seg: 15,
            envio_seg,
            idle_seg: 300,
            capturar_titulos: false, // hasta que el servidor lo permita
        }
    }

    /// Actualiza desde el objeto `config` que devuelve la edge (defensivo: si falta
    /// una clave, conserva el valor previo).
    fn aplicar(&mut self, c: &serde_json::Value) {
        if let Some(b) = c.get("seguimiento_actividad").and_then(|v| v.as_bool()) {
            self.seguimiento = b;
        }
        if let Some(b) = c.get("equipo_habilitado").and_then(|v| v.as_bool()) {
            self.equipo_habilitado = b;
        }
        if let Some(n) = c.get("muestreo_seg").and_then(|v| v.as_u64()) {
            self.muestreo_seg = n.clamp(5, 3600);
        }
        if let Some(n) = c.get("envio_seg").and_then(|v| v.as_u64()) {
            self.envio_seg = n.clamp(15, 3600);
        }
        if let Some(n) = c.get("idle_seg").and_then(|v| v.as_u64()) {
            self.idle_seg = n.clamp(30, 7200);
        }
        if let Some(b) = c.get("capturar_titulos").and_then(|v| v.as_bool()) {
            self.capturar_titulos = b;
        }
    }
}

// ── Primitivas de medición (por-sesión: sólo válidas en la sesión del usuario) ─

/// Milisegundos que el usuario lleva inactivo (sin teclado ni mouse).
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
    use winapi::um::winuser::{GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW};
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

/// IP LAN de origen HACIA el servidor de reporte. Trucos de socket UDP "connect"
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
    // (re)escribir el estado de forma atómica
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join("estado.tmp");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        if f.write_all(actual.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, &ruta);
        }
    }
    cambio
}

// ── Buffer de intervalos de actividad ────────────────────────────────────────
struct Abierto {
    inicio: chrono::DateTime<chrono::Utc>,
    fin: chrono::DateTime<chrono::Utc>,
    estado: &'static str,
    titulo: Option<String>,
}

fn cerrar(a: Abierto) -> serde_json::Value {
    serde_json::json!({
        "inicio": a.inicio.to_rfc3339(),
        "fin": a.fin.to_rfc3339(),
        "app": serde_json::Value::Null,       // Fase 2: nombre del .exe (P/Invoke extra)
        "appNombre": serde_json::Value::Null,
        "titulo": a.titulo,                    // null si capturar_titulos=false
        "url": serde_json::Value::Null,        // Fase 2: URL del navegador (UI Automation)
        "estado": a.estado,
    })
}

/// Un flush: envía el lote (o vacío = latido) y devuelve la config vigente del servidor.
fn enviar(
    cliente: &reqwest::blocking::Client,
    cfg: &Config,
    host: &str,
    ip: Option<&str>,
    so: &str,
    cambio_red: bool,
    muestras: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "dispositivo": host,
        "hostname": host,
        "ip": ip,
        "so": so,
        "agenteVersion": AGENTE_VERSION,
        "cambio_red": cambio_red,
        "muestras": muestras,
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

/// Bucle del agente. Se ejecuta en su propio hilo; nunca retorna mientras el
/// proceso viva. Si no hay config (sin URL), sale de inmediato (agente inerte).
pub fn ejecutar() {
    let cfg = match Config::cargar() {
        Some(c) => c,
        None => {
            log::info!("[monitoreo] sin DESSAU_MONITOREO_URL — agente inerte");
            return;
        }
    };
    log::info!("[monitoreo] agente {} iniciado, reporta a {}", AGENTE_VERSION, cfg.url);

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

    // Latido inmediato: DA DE ALTA el equipo (IP + hostname) apenas arranca —
    // apenas se instala / apenas se prende la PC — y trae la config vigente.
    {
        let ip = ip_local_hacia(&cfg.url);
        let cambio = detectar_cambio_red(ip.as_deref(), &host);
        match enviar(&cliente, &cfg, &host, ip.as_deref(), &so, cambio, vec![]) {
            Ok(c) => pol.aplicar(&c),
            Err(e) => log::warn!("[monitoreo] alta inicial falló: {e}"),
        }
    }

    let mut buf: Vec<serde_json::Value> = vec![];
    let mut abierto: Option<Abierto> = None;
    let mut ultimo_envio = Instant::now();

    loop {
        std::thread::sleep(Duration::from_secs(pol.muestreo_seg));

        // 1) Medir SÓLO si TI tiene el seguimiento encendido para este equipo.
        //    Si está apagado, no medimos (privacidad) pero igual latimos abajo
        //    para refrescar el alta del equipo y recibir la config.
        if pol.seguimiento && pol.equipo_habilitado {
            let idle = inactividad_ms();
            let estado: &'static str = if idle >= pol.idle_seg * 1000 { "ocioso" } else { "activo" };
            let titulo = if pol.capturar_titulos {
                let t = ventana_primer_plano();
                if t.is_empty() { None } else { Some(t) }
            } else {
                None
            };
            let ahora = chrono::Utc::now();
            let extender = matches!(&abierto, Some(a) if a.estado == estado);
            if extender {
                // mismo estado: extender el intervalo abierto
                let a = abierto.as_mut().unwrap();
                a.fin = ahora;
                a.titulo = titulo;
            } else {
                // cambió el estado: cerrar el abierto y abrir uno nuevo
                if let Some(a) = abierto.take() {
                    buf.push(cerrar(a));
                }
                abierto = Some(Abierto { inicio: ahora, fin: ahora, estado, titulo });
            }
        }

        // 2) Flush cada envio_seg (siempre: aunque no haya muestras, es el latido
        //    que mantiene el alta del equipo y trae la política actualizada).
        if ultimo_envio.elapsed().as_secs() >= pol.envio_seg {
            if let Some(a) = abierto.take() {
                buf.push(cerrar(a));
            }
            let muestras = std::mem::take(&mut buf);
            let ip = ip_local_hacia(&cfg.url);
            let cambio = detectar_cambio_red(ip.as_deref(), &host);
            match enviar(&cliente, &cfg, &host, ip.as_deref(), &so, cambio, muestras) {
                Ok(c) => pol.aplicar(&c),
                // Fase 2: en fallo, bufferear a disco y reintentar (offline). Hoy se
                // pierden las muestras del lote fallido; el alta se reintenta al próximo.
                Err(e) => log::warn!("[monitoreo] latido falló: {e}"),
            }
            ultimo_envio = Instant::now();

            // Si TI apagó el seguimiento, no acumular nada.
            if !pol.seguimiento || !pol.equipo_habilitado {
                buf.clear();
                abierto = None;
            }
        }
    }
}
