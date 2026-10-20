

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

    let mut capturer = Capturer::new(display)?;
    let (w, h, rgba) = leer_un_frame(&mut capturer)?;
    let jpeg = rgba_a_jpeg(w, h, rgba, max_ancho, difuminar)?;
    Ok(STANDARD.encode(&jpeg))
}

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
            Err(ref e) if e.kind() == WouldBlock => {  }
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
