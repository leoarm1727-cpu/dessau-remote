

#![cfg(windows)]

use crate::monitoreo_interaccion::{ContadorHandle, Contadores};
use hbb_common::log;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// `+sd2` = secreto DERIVADO del MachineGuid. Marca visible en el panel (columna
// agente_version) para distinguir los equipos ya migrados al secreto estable; el
// bulk-clear de la flota se gatea con `agente_version LIKE '%+sd2'`.
const AGENTE_VERSION: &str = concat!("dessau-monitoreo/", env!("CARGO_PKG_VERSION"), "+sd2");
const MAX_ANCHO_CAPTURA: u32 = 1600;

struct Config {
    url: String,
    captura_url: String,
    apikey: Option<String>,
    envio_seg_semilla: u64,
}

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

/// GUID estable de la instalación de Windows (`HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid`).
/// Se fija al instalar Windows y NO cambia al reinstalar el agente, borrar `ProgramData` ni
/// desinstalar — por eso sirve de ancla para un secreto por-equipo que no se pierde.
pub(crate) fn machine_guid() -> Option<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let k = hklm
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Cryptography", KEY_READ | KEY_WOW64_64KEY)
        .ok()?;
    let g: String = k.get_value("MachineGuid").ok()?;
    let g = g.trim().to_string();
    if g.is_empty() {
        None
    } else {
        Some(g)
    }
}

/// Secreto por-equipo ESTABLE y DERIVADO: `sha256(salt | MachineGuid)`.
/// Es idempotente entre reinstalaciones ⇒ tras el primer registro TOFU no vuelve a cambiar,
/// así que no dispara el 401 por secreto nuevo aunque se reinstale o se borre `ProgramData`.
/// El `salt` separa canales (actividad vs. actualización) para que sus secretos no coincidan.
pub(crate) fn secreto_estable(salt: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let guid = machine_guid()?;
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b"|");
    h.update(guid.as_bytes());
    Some(hex::encode(h.finalize()))
}

const SALT_DEVICE: &str = "dessau-monitoreo-device-v1";

fn secreto_dispositivo() -> String {
    // 1) Camino normal: secreto DERIVADO del equipo (estable, no depende de ningún archivo).
    if let Some(s) = secreto_estable(SALT_DEVICE) {
        return s;
    }
    // 2) Respaldo (MachineGuid ilegible, muy raro): el UUID en archivo de siempre.
    let ruta = dir_config().map(|d| d.join("device-secret"));
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

struct Politica {
    seguimiento: bool,
    equipo_habilitado: bool,
    capturas: bool,
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

            capturar_urls: false,
            difuminar: false,
            aviso_texto: String::new(),
            aviso_version: 1,
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

        if let Some(s) = c.get("aviso_texto").and_then(|v| v.as_str()) {
            self.aviso_texto = s.to_string();
        }
        self.aviso_version = c.get("aviso_version").and_then(|v| v.as_i64()).unwrap_or(self.aviso_version);
    }

    fn monitorea(&self) -> bool {
        self.equipo_habilitado && (self.seguimiento || (self.capturas && self.capturas_activas))
    }
}

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

fn detectar_cambio_red(ip: Option<&str>, host: &str) -> bool {
    let actual = format!("{}|{}", ip.unwrap_or(""), host);
    let Some(dir) = dir_config() else { return false; };
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

const MAX_POR_LOTE: usize = 1000;

const MAX_PENDIENTES: usize = 20000;

fn ruta_pendientes() -> Option<PathBuf> {
    dir_config().map(|d| d.join("pendiente.json"))
}

fn cargar_pendientes() -> Vec<serde_json::Value> {
    let Some(p) = ruta_pendientes() else {
        return vec![];
    };
    match std::fs::read(&p) {
        Ok(bytes) => serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).unwrap_or_default(),
        Err(_) => vec![],
    }
}

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

struct Abierto {
    inicio: chrono::DateTime<chrono::Utc>,
    fin: chrono::DateTime<chrono::Utc>,
    estado: &'static str,
    titulo: Option<String>,
    app: Option<String>,
    url: Option<String>,

    inter: Option<Contadores>,
}

fn cerrar(a: Abierto) -> serde_json::Value {
    let app_nombre = a.app.as_deref().map(nombre_amigable_app);
    serde_json::json!({
        "inicio": a.inicio.to_rfc3339(),
        "fin": a.fin.to_rfc3339(),
        "app": a.app,
        "appNombre": app_nombre,
        "titulo": a.titulo,
        "url": a.url,
        "estado": a.estado,

        "pulsaciones": a.inter.map(|c| c.pulsaciones),
        "clics": a.inter.map(|c| c.clics),
        "segMovimiento": a.inter.map(|c| c.seg_movimiento),
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

    let mut buf: Vec<serde_json::Value> = cargar_pendientes();
    if !buf.is_empty() {
        log::info!("[monitoreo] {} muestras pendientes recuperadas del disco", buf.len());
    }
    let mut abierto: Option<Abierto> = None;
    let mut _indicador: Option<()> = None;
    let mut contador: Option<ContadorHandle> = None;

    let mut ultimo_intento_contador: Option<Instant> = None;
    const REINTENTO_CONTADOR_SEG: u64 = 300;
    let mut ultimo_envio = Instant::now();
    let mut ultima_captura = Instant::now();
    let mut primera = true;

    // Latido inmediato ANTES del primer tick: trae la config remota (seguimiento/capturas)
    // para que la primera actividad y la primera captura salgan APENAS INSTALAR, en vez de
    // esperar un intervalo completo. Sin esto, la política semilla (capturas/seguimiento en
    // off) bloquea la captura de la 1ª iteración y la config recién se aplica después, así
    // que la primera captura tardaba ~1-2 min. Si falla (p. ej. 401), se ignora y el loop
    // sigue como siempre — no hay regresión.
    {
        let ip = ip_local_hacia(&cfg.url);
        if let Ok(c) = enviar_actividad(&cliente, &cfg, &secret, &host, ip.as_deref(), &so, false, &[]) {
            pol.aplicar(&c);
        }
    }

    loop {

        let quiere_monitorear = pol.monitorea();
        let consentido = if quiere_monitorear {
            true
        } else {
            false
        };
        let activo = quiere_monitorear && consentido;

        if activo && pol.seguimiento {

            if contador.as_ref().map(|c| !c.activo()).unwrap_or(false) {
                if let Some(c) = contador.take() {
                    c.detener();
                }
            }
            let toca_reintentar = ultimo_intento_contador
                .map(|t| t.elapsed().as_secs() >= REINTENTO_CONTADOR_SEG)
                .unwrap_or(true);
            if contador.is_none() && toca_reintentar {
                ultimo_intento_contador = Some(Instant::now());
                contador = Some(crate::monitoreo_interaccion::iniciar_contador());
            }
        } else {
            if let Some(c) = contador.take() {
                c.detener();
            }
            ultimo_intento_contador = None;
        }

        let inter_ok = contador.as_ref().map(|c| c.activo()).unwrap_or(false);

        if activo && pol.seguimiento {
            let idle = inactividad_ms();
            let estado: &'static str = if idle >= pol.idle_seg * 1000 {
                "ocioso"
            } else {
                "activo"
            };

            let app = app_primer_plano();
            let titulo = if !pol.capturar_titulos {
                None
            } else {
                let t = ventana_primer_plano();
                if t.is_empty() { None } else { Some(t) }
            };

            let url = if !pol.capturar_urls {
                None
            } else {
                app.as_deref().and_then(crate::monitoreo_url::url_de_navegador)
            };
            let ahora = chrono::Utc::now();

            let delta = crate::monitoreo_interaccion::tomar();
            if let Some(a) = abierto.as_mut() {

                if !inter_ok {
                    a.inter = None;
                } else if let Some(acum) = a.inter.as_mut() {
                    acum.sumar(delta);
                }
            }

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
                abierto = Some(Abierto {
                    inicio: ahora,
                    fin: ahora,
                    estado,
                    titulo,
                    app,
                    url,
                    inter: if inter_ok {
                        Some(Contadores::default())
                    } else {
                        None
                    },
                });
            }
        }

        if activo
            && pol.capturas
            && pol.capturas_activas
            && (primera || ultima_captura.elapsed().as_secs() >= pol.captura_intervalo_seg)
        {
            let imagenes = crate::monitoreo_captura::capturar_monitores(MAX_ANCHO_CAPTURA, pol.difuminar);
            enviar_capturas(&cliente, &cfg, &secret, &host, imagenes);
            ultima_captura = Instant::now();
        }

        if primera || ultimo_envio.elapsed().as_secs() >= pol.envio_seg {
            if let Some(mut a) = abierto.take() {

                let cola = crate::monitoreo_interaccion::tomar();
                if let Some(acum) = a.inter.as_mut() {
                    acum.sumar(cola);
                }
                buf.push(cerrar(a));
            }

            if buf.len() > MAX_PENDIENTES {
                let sobran = buf.len() - MAX_PENDIENTES;
                buf.drain(0..sobran);
            }
            let ip = ip_local_hacia(&cfg.url);
            let cambio = detectar_cambio_red(ip.as_deref(), &host);

            let mut primer_lote = true;
            loop {
                let n = buf.len().min(MAX_POR_LOTE);
                if n == 0 && !primer_lote {
                    break;
                }
                let lote: Vec<serde_json::Value> = buf.drain(0..n).collect();

                let cambio_lote = cambio && primer_lote;
                match enviar_actividad(
                    &cliente, &cfg, &secret, &host, ip.as_deref(), &so, cambio_lote, &lote,
                ) {
                    Ok(c) => pol.aplicar(&c),
                    Err(e) => {
                        log::warn!("[monitoreo] latido falló (se reintenta luego): {e}");

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

            if buf.is_empty() {
                guardar_pendientes(&[]);
            }
            ultimo_envio = Instant::now();
        }

        primera = false;
        std::thread::sleep(Duration::from_secs(pol.muestreo_seg));
    }
}
