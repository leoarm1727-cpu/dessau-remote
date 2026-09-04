// =============================================================================
//  Agente de monitoreo de productividad Dessau — TRANSPARENTE (Ley 29733)
// -----------------------------------------------------------------------------
//  ⚠️ AVISO AGPL-3.0: modificación del cliente RustDesk. Ver LEEME-FORK.md.
//
//  Corre en el proceso `--tray` (sesión interactiva del usuario), único lugar que
//  ve el escritorio. Es TRANSPARENTE: NO mide ni captura nada sin CONSENTIMIENTO
//  informado (monitoreo_aviso), y mientras monitorea muestra un INDICADOR VISIBLE
//  permanente (monitoreo_indicador). Sin ocultamiento ni evasión de antivirus.
//
//  Autenticación por SECRETO POR DISPOSITIVO: cada PC genera el suyo (no hay
//  secreto compartido en el config); la edge lo valida por-dispositivo (TOFU).
//
//  Qué hace, según la política que devuelve el servidor:
//   1. REGISTRO: late con identidad (hostname+IP+SO); el backend detecta cambios.
//   2. ACTIVIDAD (si `seguimiento_actividad`): intervalos activo/ocioso + título.
//   3. CAPTURA (si `capturas_activas`): screenshot por monitor -> edge de capturas.
//  Apagar el monitoreo desde Admin lo apaga en el siguiente latido (y oculta el
//  indicador). La política vive en el servidor, no en el agente.
// =============================================================================
#![cfg(windows)]

use crate::monitoreo_indicador::IndicadorHandle;
use hbb_common::log;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const AGENTE_VERSION: &str = concat!("dessau-monitoreo/", env!("CARGO_PKG_VERSION"));
const MAX_ANCHO_CAPTURA: u32 = 1600;

// ── Config (solo URL + apikey; el secreto ya NO va en el config) ─────────────
struct Config {
    url: String,             // edge de actividad
    captura_url: String,     // derivada: edge de capturas
    apikey: Option<String>,  // opcional (si la edge exige apikey/JWT)
    envio_seg_semilla: u64,
}

/// %ProgramData%\Dessau\monitoreo — config (lo escribe el instalador, solo lectura).
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

impl Config {
    fn cargar() -> Option<Self> {
        let kv = dir_config()
            .map(|d| leer_kv(&d.join("config")))
            .unwrap_or_default();
        let get = |k: &str| -> Option<String> {
            kv.get(k)
                .cloned()
                .or_else(|| std::env::var(k).ok())
                .filter(|s| !s.is_empty())
        };
        let url = get("DESSAU_MONITOREO_URL")?;
        let captura_url = url.replace("monitoreo-actividad-device", "monitoreo-captura-device");
        let apikey = get("DESSAU_MONITOREO_APIKEY");
        let envio_seg_semilla = get("DESSAU_MONITOREO_INTERVALO")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n >= 5)
            .unwrap_or(60);
        Some(Config { url, captura_url, apikey, envio_seg_semilla })
    }
}

/// Secreto POR DISPOSITIVO. Se genera una vez y se guarda en %APPDATA% (escribible
/// por el usuario). Un usuario que lo lea sólo puede afectar SU propio equipo.
fn secreto_dispositivo() -> String {
    let ruta = monitoreo_aviso_dir().map(|d| d.join("device-secret"));
    if let Some(r) = &ruta {
        if let Ok(s) = std::fs::read_to_string(r) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    let nuevo = uuid::Uuid::new_v4().simple().to_string();
    if let Some(r) = &ruta {
        if let Some(dir) = r.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(r, &nuevo);
    }
    nuevo
}

fn monitoreo_aviso_dir() -> Option<PathBuf> {
    crate::monitoreo_aviso::dir_usuario()
}

// ── Política vigente (la manda el servidor) ──────────────────────────────────
struct Politica {
    seguimiento: bool,
    equipo_habilitado: bool,
    capturas: bool,        // interruptor maestro de capturas (`activo`)
    capturas_activas: bool,
    muestreo_seg: u64,
    envio_seg: u64,
    idle_seg: u64,
    captura_intervalo_seg: u64,
    capturar_titulos: bool,
    capturar_urls: bool,
    difuminar: bool,
    aviso_texto: String,
    aviso_version: i64,
    tiempo_privado_permitido: bool,
    tiempo_privado_max_min: u64,
}

impl Politica {
    fn semilla(envio_seg: u64) -> Self {
        Politica {
            seguimiento: false,
            equipo_habilitado: true,
            capturas: false,
            capturas_activas: false,
            muestreo_seg: 15,
            envio_seg,
            idle_seg: 300,
            captura_intervalo_seg: 300,
            capturar_titulos: false,
            // URL del navegador: OPT-IN. Arranca apagada; solo se activa si TI la
            // enciende explícitamente en la config del servidor (`capturar_urls`).
            capturar_urls: false,
            difuminar: false,
            aviso_texto: String::new(),
            aviso_version: 1,
            tiempo_privado_permitido: false,
            tiempo_privado_max_min: 60,
        }
    }

    fn aplicar(&mut self, c: &serde_json::Value) {
        let b = |k: &str, d: bool| c.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let n = |k: &str, d: u64| c.get(k).and_then(|v| v.as_u64()).unwrap_or(d);
        self.seguimiento = b("seguimiento_actividad", self.seguimiento);
        self.equipo_habilitado = b("equipo_habilitado", self.equipo_habilitado);
        self.capturas = b("activo", self.capturas);
        self.capturas_activas = b("capturas_activas", self.capturas_activas);
        self.muestreo_seg = n("muestreo_seg", self.muestreo_seg).clamp(5, 3600);
        self.envio_seg = n("envio_seg", self.envio_seg).clamp(15, 3600);
        self.idle_seg = n("idle_seg", self.idle_seg).clamp(30, 7200);
        self.captura_intervalo_seg = n("intervalo_seg", self.captura_intervalo_seg).clamp(30, 7200);
        self.capturar_titulos = b("capturar_titulos", self.capturar_titulos);
        self.capturar_urls = b("capturar_urls", self.capturar_urls);
        self.difuminar = b("difuminar_capturas", self.difuminar);
        self.tiempo_privado_permitido = b("tiempo_privado_permitido", self.tiempo_privado_permitido);
        self.tiempo_privado_max_min =
            n("tiempo_privado_max_min", self.tiempo_privado_max_min).clamp(5, 480);
        if let Some(s) = c.get("aviso_texto").and_then(|v| v.as_str()) {
            self.aviso_texto = s.to_string();
        }
        self.aviso_version = c.get("aviso_version").and_then(|v| v.as_i64()).unwrap_or(self.aviso_version);
    }

    /// ¿Debe monitorear (actividad y/o captura) este equipo ahora?
    fn monitorea(&self) -> bool {
        self.equipo_habilitado && (self.seguimiento || (self.capturas && self.capturas_activas))
    }
}

// ── Primitivas de medición (sólo válidas en la sesión interactiva) ───────────
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
        GetTickCount().wrapping_sub(lii.dwTime) as u64
    }
}

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

/// Nombre del ejecutable (.exe) de la ventana en primer plano, en minúsculas
/// (ej. "code.exe"). Es lo que clasifica el catálogo `monitoreo_apps` (compara
/// `app.toLowerCase() === patron`, donde los patrones son "code.exe", "chrome.exe"…).
/// Sin esto el índice de productividad queda en 0% (todo "Sin categoría").
fn app_primer_plano() -> Option<String> {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
    use winapi::um::winuser::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf: Vec<u16> = vec![0u16; 512];
        let mut size: u32 = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok == 0 || size == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        let base = full
            .rsplit(|c| c == '\\' || c == '/')
            .next()
            .unwrap_or(&full)
            .trim()
            .to_lowercase();
        if base.is_empty() {
            None
        } else {
            Some(base)
        }
    }
}

/// Nombre amigable para mostrar en el panel ("code.exe" -> "Code").
fn nombre_amigable_app(exe: &str) -> String {
    let base = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut chars = base.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => base.to_string(),
    }
}

fn hostname() -> String {
    hbb_common::whoami::fallible::hostname().unwrap_or_else(|_| "desconocido".into())
}

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

fn ip_local_hacia(endpoint: &str) -> Option<String> {
    use std::net::{ToSocketAddrs, UdpSocket};
    let host = url::Url::parse(endpoint).ok()?.host_str()?.to_string();
    let puerto = if endpoint.starts_with("http://") { 80 } else { 443 };
    let addr = format!("{host}:{puerto}").to_socket_addrs().ok()?.next()?;
    let sock = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.connect(addr).ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

/// Cambio de IP/hostname en archivo de estado (por usuario, %APPDATA%).
fn detectar_cambio_red(ip: Option<&str>, host: &str) -> bool {
    let actual = format!("{}|{}", ip.unwrap_or(""), host);
    let Some(dir) = monitoreo_aviso_dir() else { return false; };
    let ruta = dir.join("estado");
    let previo = std::fs::read_to_string(&ruta).ok();
    let cambio = matches!(&previo, Some(p) if p.trim() != actual);
    if cambio || previo.is_none() {
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join("estado.tmp");
        if let Ok(()) = std::fs::write(&tmp, &actual) {
            let _ = std::fs::rename(&tmp, &ruta);
        }
    }
    cambio
}

// ── Buffer offline (sobrevive cortes de red y reinicios) ─────────────────────
/// Máximo por lote enviado al edge (su tope es 2000; dejamos margen).
const MAX_POR_LOTE: usize = 1000;
/// Tope del buffer en disco: si el equipo estuvo mucho tiempo offline, se
/// conservan las muestras MÁS RECIENTES hasta este número (evita crecer sin fin).
const MAX_PENDIENTES: usize = 20000;

fn ruta_pendientes() -> Option<PathBuf> {
    monitoreo_aviso_dir().map(|d| d.join("pendiente.json"))
}

/// Carga el buffer de muestras que quedaron sin enviar (por red caída o cierre).
fn cargar_pendientes() -> Vec<serde_json::Value> {
    let Some(p) = ruta_pendientes() else {
        return vec![];
    };
    match std::fs::read(&p) {
        Ok(bytes) => serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).unwrap_or_default(),
        Err(_) => vec![],
    }
}

/// Persiste (o borra si está vacío) el buffer pendiente en disco, de forma atómica.
fn guardar_pendientes(muestras: &[serde_json::Value]) {
    let Some(p) = ruta_pendientes() else {
        return;
    };
    if muestras.is_empty() {
        let _ = std::fs::remove_file(&p);
        return;
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_vec(muestras) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, &txt).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

// ── Buffer de intervalos de actividad ────────────────────────────────────────
struct Abierto {
    inicio: chrono::DateTime<chrono::Utc>,
    fin: chrono::DateTime<chrono::Utc>,
    estado: &'static str,
    titulo: Option<String>,
    app: Option<String>,
    url: Option<String>,
}

fn cerrar(a: Abierto) -> serde_json::Value {
    let app_nombre = a.app.as_deref().map(nombre_amigable_app);
    serde_json::json!({
        "inicio": a.inicio.to_rfc3339(),
        "fin": a.fin.to_rfc3339(),
        "app": a.app,                          // ej. "code.exe" (clasifica el catálogo)
        "appNombre": app_nombre,               // ej. "Code" (para mostrar en el panel)
        "titulo": a.titulo,
        "url": a.url,                          // dominio -> clasifica navegación (opt-in)
        "estado": a.estado,
    })
}

fn con_auth(
    mut req: reqwest::blocking::RequestBuilder,
    secret: &str,
    apikey: &Option<String>,
) -> reqwest::blocking::RequestBuilder {
    req = req.header("x-device-secret", secret);
    if let Some(k) = apikey {
        req = req.header("apikey", k).header("Authorization", format!("Bearer {k}"));
    }
    req
}

/// Latido de actividad/registro. Devuelve la config vigente del servidor.
fn enviar_actividad(
    cliente: &reqwest::blocking::Client,
    cfg: &Config,
    secret: &str,
    host: &str,
    ip: Option<&str>,
    so: &str,
    cambio_red: bool,
    muestras: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "dispositivo": host, "hostname": host, "ip": ip, "so": so,
        "agenteVersion": AGENTE_VERSION, "cambio_red": cambio_red, "muestras": muestras,
    });
    let req = con_auth(cliente.post(&cfg.url).json(&payload), secret, &cfg.apikey);
    let resp = req.send().map_err(|e| format!("POST falló: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().map_err(|e| format!("no-JSON: {e}"))?;
    Ok(body.get("config").cloned().unwrap_or(serde_json::Value::Null))
}

/// Sube las capturas (base64) a la edge de capturas.
fn enviar_capturas(
    cliente: &reqwest::blocking::Client,
    cfg: &Config,
    secret: &str,
    host: &str,
    imagenes: Vec<String>,
) {
    if imagenes.is_empty() {
        return;
    }
    let payload = serde_json::json!({
        "dispositivo": host, "hostname": host, "appVersion": AGENTE_VERSION,
        "capturadoEn": chrono::Utc::now().to_rfc3339(), "imagenes": imagenes,
    });
    let req = con_auth(cliente.post(&cfg.captura_url).json(&payload), secret, &cfg.apikey);
    match req.send() {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => log::warn!("[monitoreo] captura HTTP {}", r.status()),
        Err(e) => log::warn!("[monitoreo] captura falló: {e}"),
    }
}

/// Bucle del agente (hilo del proceso --tray). Sin panic (perfil panic=abort).
pub fn ejecutar() {
    let Some(cfg) = Config::cargar() else {
        log::info!("[monitoreo] sin DESSAU_MONITOREO_URL — agente inerte");
        return;
    };
    let secret = secreto_dispositivo();
    log::info!("[monitoreo] agente {AGENTE_VERSION} iniciado (transparente), reporta a {}", cfg.url);

    let Ok(cliente) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    else {
        log::error!("[monitoreo] no se pudo crear el cliente HTTP");
        return;
    };

    let host = hostname();
    let so = so_windows();
    let mut pol = Politica::semilla(cfg.envio_seg_semilla);

    // Buffer offline: arranca con lo que haya quedado sin enviar de una corrida
    // anterior (red caída, reinicio, apagón). Así no se pierde actividad.
    let mut buf: Vec<serde_json::Value> = cargar_pendientes();
    if !buf.is_empty() {
        log::info!("[monitoreo] {} muestras pendientes recuperadas del disco", buf.len());
    }
    let mut abierto: Option<Abierto> = None;
    let mut indicador: Option<IndicadorHandle> = None;
    let mut ultimo_envio = Instant::now();
    let mut ultima_captura = Instant::now();
    let mut primera = true;

    // Tiempo privado: el trabajador lo alterna con un clic en el indicador (banner).
    // El flag es compartido (Arc<AtomicBool>) con ese hilo. Mientras está en true,
    // app/título/URL se apagan ACÁ (el agente, no solo el edge) y se manda
    // estado="privado" — minimización en origen, defensa en profundidad.
    let privado_flag = Arc::new(AtomicBool::new(false));
    let mut privado_desde: Option<Instant> = None;

    loop {
        // 1) ¿monitorear? Requiere política ON + CONSENTIMIENTO informado vigente.
        let quiere_monitorear = pol.monitorea();
        let consentido = if quiere_monitorear {
            crate::monitoreo_aviso::asegurar_consentimiento(&pol.aviso_texto, pol.aviso_version)
        } else {
            false
        };
        let activo = quiere_monitorear && consentido;

        // Si TI apagó el permiso de tiempo privado (o el monitoreo), no dejar el
        // flag trabado en true de una sesión anterior.
        if !pol.tiempo_privado_permitido || !activo {
            if privado_flag.swap(false, Ordering::SeqCst) {
                privado_desde = None;
            }
        }

        // Auto-reanudar si superó el máximo permitido (evita "me olvidé de reanudar").
        if privado_flag.load(Ordering::SeqCst) {
            if privado_desde.is_none() {
                privado_desde = Some(Instant::now());
            }
            if let Some(t0) = privado_desde {
                if t0.elapsed().as_secs() >= pol.tiempo_privado_max_min * 60 {
                    privado_flag.store(false, Ordering::SeqCst);
                    privado_desde = None;
                    log::info!("[monitoreo] tiempo privado auto-reanudado (tope de {} min)", pol.tiempo_privado_max_min);
                    if let Some(h) = &indicador {
                        h.repintar();
                    }
                }
            }
        } else {
            privado_desde = None;
        }

        // 2) Indicador visible: encendido sólo mientras se monitorea de verdad.
        // Se recrea si cambió el permiso de tiempo privado (el banner necesita
        // saberlo para aceptar o ignorar el clic).
        let permiso_cambio = indicador
            .as_ref()
            .map(|h| h.permitido_pausa() != pol.tiempo_privado_permitido)
            .unwrap_or(false);
        if permiso_cambio {
            if let Some(h) = indicador.take() {
                h.detener();
            }
        }
        if activo && indicador.is_none() {
            indicador = Some(crate::monitoreo_indicador::iniciar_indicador(
                privado_flag.clone(),
                pol.tiempo_privado_permitido,
            ));
        } else if !activo {
            if let Some(h) = indicador.take() {
                h.detener();
            }
        }

        // 3) Muestrear actividad (sólo si activo && seguimiento).
        if activo && pol.seguimiento {
            let en_privado = pol.tiempo_privado_permitido && privado_flag.load(Ordering::SeqCst);
            let idle = inactividad_ms();
            let estado: &'static str = if en_privado {
                "privado"
            } else if idle >= pol.idle_seg * 1000 {
                "ocioso"
            } else {
                "activo"
            };
            // En tiempo privado no se lee NADA de contenido (defensa en profundidad;
            // el edge también lo anula, pero acá ni se intenta leer). El .exe se
            // captura siempre que haya seguimiento (es la base de la clasificación de
            // productividad); el título va gateado por capturar_titulos.
            let app = if en_privado { None } else { app_primer_plano() };
            let titulo = if en_privado || !pol.capturar_titulos {
                None
            } else {
                let t = ventana_primer_plano();
                if t.is_empty() { None } else { Some(t) }
            };
            // URL: solo si TI la habilitó (opt-in) y no está en privado.
            // url_de_navegador ya devuelve None si el .exe no es un navegador soportado.
            let url = if en_privado || !pol.capturar_urls {
                None
            } else {
                app.as_deref().and_then(crate::monitoreo_url::url_de_navegador)
            };
            let ahora = chrono::Utc::now();
            // Se corta el intervalo cuando cambia el estado, la app O la URL, así el
            // panel reparte el tiempo por programa y por sitio (no lo agrupa todo).
            let extender =
                matches!(&abierto, Some(a) if a.estado == estado && a.app == app && a.url == url);
            if extender {
                if let Some(a) = abierto.as_mut() {
                    a.fin = ahora;
                    a.titulo = titulo;
                }
            } else {
                if let Some(a) = abierto.take() {
                    buf.push(cerrar(a));
                }
                abierto = Some(Abierto { inicio: ahora, fin: ahora, estado, titulo, app, url });
            }
        }

        // 4) Captura de pantalla (sólo si activo && capturas), cada intervalo.
        if activo
            && pol.capturas
            && pol.capturas_activas
            && (primera || ultima_captura.elapsed().as_secs() >= pol.captura_intervalo_seg)
        {
            let imagenes = crate::monitoreo_captura::capturar_monitores(MAX_ANCHO_CAPTURA, pol.difuminar);
            enviar_capturas(&cliente, &cfg, &secret, &host, imagenes);
            ultima_captura = Instant::now();
        }

        // 5) Latido de actividad/registro cada envio_seg (y el primero de una).
        if primera || ultimo_envio.elapsed().as_secs() >= pol.envio_seg {
            if let Some(a) = abierto.take() {
                buf.push(cerrar(a));
            }
            // Si estuvimos offline, quedarnos con las muestras MÁS RECIENTES.
            if buf.len() > MAX_PENDIENTES {
                let sobran = buf.len() - MAX_PENDIENTES;
                buf.drain(0..sobran);
            }
            let ip = ip_local_hacia(&cfg.url);
            let cambio = detectar_cambio_red(ip.as_deref(), &host);
            // Enviar por lotes hasta vaciar el buffer o hasta que un envío falle.
            // Siempre se hace AL MENOS un envío (aunque el buffer esté vacío) para
            // registrar el equipo y refrescar la config. Si un lote falla, se
            // devuelve al frente del buffer y se persiste a disco para reintentar.
            let mut primer_lote = true;
            loop {
                let n = buf.len().min(MAX_POR_LOTE);
                if n == 0 && !primer_lote {
                    break;
                }
                let lote: Vec<serde_json::Value> = buf.drain(0..n).collect();
                // `cambio_red` solo en el primer lote (evita avisos duplicados).
                let cambio_lote = cambio && primer_lote;
                match enviar_actividad(
                    &cliente, &cfg, &secret, &host, ip.as_deref(), &so, cambio_lote, &lote,
                ) {
                    Ok(c) => pol.aplicar(&c),
                    Err(e) => {
                        log::warn!("[monitoreo] latido falló (se reintenta luego): {e}");
                        // Devolver el lote al FRENTE, preservando el orden.
                        for (i, m) in lote.into_iter().enumerate() {
                            buf.insert(i, m);
                        }
                        guardar_pendientes(&buf);
                        break;
                    }
                }
                primer_lote = false;
                if buf.is_empty() {
                    break;
                }
            }
            // Si se vació todo, limpiar el respaldo en disco.
            if buf.is_empty() {
                guardar_pendientes(&[]);
            }
            ultimo_envio = Instant::now();
        }

        primera = false;
        std::thread::sleep(Duration::from_secs(pol.muestreo_seg));
    }
}
