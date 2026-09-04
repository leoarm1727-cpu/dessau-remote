// =============================================================================
//  Captura de pantalla — monitoreo de productividad TRANSPARENTE (Ley 29733)
// -----------------------------------------------------------------------------
//  ⚖️ Esta función SOLO se invoca cuando el trabajador YA dio su consentimiento
//  informado (ver monitoreo_aviso.rs) y mientras el INDICADOR VISIBLE está en
//  pantalla mostrándole que el monitoreo está activo. No hay ocultamiento ni
//  evasión de antivirus: el empleado sabe y ve que esto corre. Si TI apaga el
//  seguimiento o el trabajador no consiente, esto NO se ejecuta.
//
//  Reusa SOLO deps ya presentes: scrap (la captura del propio RustDesk), image
//  0.24 (JPEG) y hbb_common (base64). Patrón adaptado de src/server/video_service.rs
//  (get_rgba_from_pixelbuf + el loop WouldBlock→GDI). ⚠️ AGPL: ver LEEME-FORK.md.
// =============================================================================
#![cfg(windows)]

use hbb_common::{
    bail,
    base64::{engine::general_purpose::STANDARD, Engine as _},
    log, ResultType,
};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, ColorType, DynamicImage, RgbaImage};
use scrap::{Capturer, Display, Frame, Pixfmt, TraitCapturer, TraitPixelBuffer};
use std::{io::ErrorKind::WouldBlock, time::Duration};

const CALIDAD_JPEG: u8 = 60;
const TIMEOUT_FRAME: Duration = Duration::from_millis(200);
const MAX_INTENTOS: u32 = 30;

/// Devuelve un JPEG en base64 por monitor (uno por display, en orden). Nunca hace
/// panic: si un monitor falla se registra y se omite; el resto se devuelve igual.
/// `max_ancho == 0` desactiva el redimensionado. LLAMAR SOLO con consentimiento
/// vigente y el indicador visible en pantalla (ver monitoreo.rs / monitoreo_aviso.rs).
pub fn capturar_monitores(max_ancho: u32, difuminar: bool) -> Vec<String> {
    let displays = match Display::all() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[monitoreo] Display::all() falló: {e}");
            return Vec::new();
        }
    };

    let mut salida = Vec::with_capacity(displays.len());
    for (idx, display) in displays.into_iter().enumerate() {
        match capturar_uno(display, max_ancho, difuminar) {
            Ok(b64) => salida.push(b64),
            Err(e) => log::error!("[monitoreo] captura monitor {idx} falló: {e}"),
        }
    }
    salida
}

fn capturar_uno(display: Display, max_ancho: u32, difuminar: bool) -> ResultType<String> {
    // Recrear el Capturer en cada captura: un cambio de resolución o de escritorio
    // (DXGI_ERROR_ACCESS_LOST) no deja un capturer muerto.
    let mut capturer = Capturer::new(display)?;
    let (w, h, rgba) = leer_un_frame(&mut capturer)?;
    let jpeg = rgba_a_jpeg(w, h, rgba, max_ancho, difuminar)?;
    Ok(STANDARD.encode(&jpeg))
}

/// Lee UN frame con reintentos. En pantalla estática DXGI no entrega frame hasta
/// que algo cambia (WouldBlock), así que tras varios intentos se fuerza GDI (toma
/// el framebuffer al instante). Mismo patrón que src/server/video_service.rs.
fn leer_un_frame(capturer: &mut Capturer) -> ResultType<(u32, u32, Vec<u8>)> {
    let mut intentos = 0u32;
    loop {
        match capturer.frame(TIMEOUT_FRAME) {
            Ok(frame) => {
                if frame.valid() {
                    match frame {
                        Frame::PixelBuffer(pb) => {
                            let w = pb.width() as u32;
                            let h = pb.height() as u32;
                            let rgba = pixelbuf_a_rgba(&pb)?;
                            return Ok((w, h, rgba));
                        }
                        Frame::Texture(_) => bail!("frame de textura (VRAM) no soportado"),
                    }
                }
            }
            Err(ref e) if e.kind() == WouldBlock => { /* reintentar */ }
            Err(e) => return Err(e.into()),
        }

        intentos += 1;
        if intentos == 4 && !capturer.is_gdi() {
            log::info!("[monitoreo] sin imagen por DXGI, forzando GDI");
            capturer.set_gdi();
        }
        if intentos >= MAX_INTENTOS {
            bail!("sin frame tras {MAX_INTENTOS} intentos");
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// BGRA (con posible padding de stride) -> RGBA compacto. Adaptado de
/// video_service.rs::get_rgba_from_pixelbuf, con guarda de límites.
fn pixelbuf_a_rgba(pixbuf: &scrap::PixelBuffer) -> ResultType<Vec<u8>> {
    let w = pixbuf.width();
    let h = pixbuf.height();
    let Some(s) = pixbuf.stride().get(0).copied() else {
        bail!("stride vacío");
    };

    if s == w * 4 {
        let mut rgba = Vec::new();
        scrap::convert(pixbuf, Pixfmt::RGBA, &mut rgba)?;
        Ok(rgba)
    } else {
        let bgra = pixbuf.data();
        if bgra.len() < s * h {
            bail!("buffer corto: {} < {}", bgra.len(), s * h);
        }
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let i = s * y + 4 * x;
                out.extend_from_slice(&[bgra[i + 2], bgra[i + 1], bgra[i], bgra[i + 3]]);
            }
        }
        Ok(out)
    }
}

/// RGBA -> (resize opcional) -> (blur opcional) -> JPEG en memoria.
fn rgba_a_jpeg(
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    max_ancho: u32,
    difuminar: bool,
) -> ResultType<Vec<u8>> {
    let Some(img) = RgbaImage::from_raw(w, h, rgba) else {
        bail!("RgbaImage::from_raw: tamaño inconsistente ({w}x{h})");
    };
    let mut din = DynamicImage::ImageRgba8(img);

    if max_ancho > 0 && din.width() > max_ancho {
        let nh = ((max_ancho as u64 * din.height() as u64) / din.width() as u64).max(1) as u32;
        din = din.resize(max_ancho, nh, FilterType::Triangle);
    }

    // Difuminado (política del servidor): bajar a ~1/14 y volver a subir.
    if difuminar {
        let (fw, fh) = (din.width(), din.height());
        let pw = (fw / 14).max(1);
        let ph = (fh / 14).max(1);
        din = din
            .resize_exact(pw, ph, FilterType::Triangle)
            .resize_exact(fw, fh, FilterType::Triangle);
    }

    let rgb = din.to_rgb8();
    let (rw, rh) = (rgb.width(), rgb.height());
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut enc = JpegEncoder::new_with_quality(&mut buf, CALIDAD_JPEG);
        enc.encode(rgb.as_raw(), rw, rh, ColorType::Rgb8)?;
    }
    Ok(buf)
}
