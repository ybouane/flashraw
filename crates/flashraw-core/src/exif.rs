//! Standard EXIF/TIFF metadata extraction into [`RawMetadata`].
//!
//! Only tags that are cheap to read are extracted here; vendor maker notes are
//! parsed by the per-format modules.

use crate::metadata::{GpsInfo, RawMetadata};
use crate::tiff::{Ifd, tags};
use crate::types::Orientation;

/// Fill basic camera information from a TIFF IFD (typically IFD0).
pub fn read_ifd0(ifd: &Ifd, meta: &mut RawMetadata) {
    if meta.make.is_empty() {
        if let Some(s) = ifd.string(tags::MAKE) {
            meta.make = s;
        }
    }
    if meta.model.is_empty() {
        if let Some(s) = ifd.string(tags::MODEL) {
            meta.model = s;
        }
    }
    if let Some(o) = ifd.u32(tags::ORIENTATION) {
        meta.orientation = Orientation::from_exif(o);
    }
    if meta.software.is_none() {
        meta.software = ifd.string(tags::SOFTWARE);
    }
    if meta.artist.is_none() {
        meta.artist = ifd.string(tags::ARTIST);
    }
    if meta.copyright.is_none() {
        meta.copyright = ifd.string(tags::COPYRIGHT);
    }
    if meta.capture_time.is_none() {
        meta.capture_time = ifd.string(tags::DATE_TIME).and_then(|s| normalize_datetime(&s, None, None));
    }
    if let Some(e) = &ifd.exif {
        read_exif(e, meta);
    }
    if let Some(g) = &ifd.gps {
        meta.gps = read_gps(g);
    }
    // TIFF-EP cameras sometimes store exposure tags directly in IFD0.
    read_exif(ifd, meta);
}

/// Fill exposure/lens/time information from an EXIF IFD.
pub fn read_exif(e: &Ifd, meta: &mut RawMetadata) {
    let x = &mut meta.exposure;
    if x.exposure_time.is_none() {
        x.exposure_time = e.f64(tags::EXPOSURE_TIME).filter(|v| *v > 0.0);
    }
    if x.f_number.is_none() {
        x.f_number = e.f64(tags::F_NUMBER).map(|v| v as f32).filter(|v| *v > 0.0);
    }
    if x.iso.is_none() {
        x.iso = e.get(tags::ISO).and_then(|v| v.u32(0)).filter(|v| *v > 0);
    }
    if x.focal_length.is_none() {
        x.focal_length = e.f64(tags::FOCAL_LENGTH).map(|v| v as f32).filter(|v| *v > 0.0);
    }
    if x.focal_length_35mm.is_none() {
        x.focal_length_35mm = e.u32(tags::FOCAL_LENGTH_35MM).filter(|v| *v > 0);
    }
    if x.exposure_bias.is_none() {
        x.exposure_bias = e.f64(tags::EXPOSURE_BIAS).map(|v| v as f32);
    }
    if x.flash.is_none() {
        x.flash = e.u32(tags::FLASH).map(|v| v as u16);
    }
    if x.metering_mode.is_none() {
        x.metering_mode = e.u32(tags::METERING_MODE).map(|v| v as u16);
    }
    if x.exposure_program.is_none() {
        x.exposure_program = e.u32(tags::EXPOSURE_PROGRAM).map(|v| v as u16);
    }
    if let Some(s) = e.string(tags::DATE_TIME_ORIGINAL) {
        let sub = e.string(tags::SUBSEC_TIME_ORIGINAL);
        let off = e.string(tags::OFFSET_TIME_ORIGINAL);
        if let Some(t) = normalize_datetime(&s, sub.as_deref(), off.as_deref()) {
            meta.capture_time = Some(t);
        }
    }
    if meta.serial.is_none() {
        meta.serial = e.string(tags::BODY_SERIAL);
    }
    let l = &mut meta.lens;
    if l.make.is_none() {
        l.make = e.string(tags::LENS_MAKE);
    }
    if l.model.is_none() {
        l.model = e.string(tags::LENS_MODEL);
    }
    if l.serial.is_none() {
        l.serial = e.string(tags::LENS_SERIAL);
    }
    if l.focal_range.is_none() {
        if let Some(spec) = e.get(tags::LENS_SPEC) {
            let v = spec.f64s();
            if v.len() >= 4 && v[0] > 0.0 {
                l.focal_range = Some((v[0] as f32, v[1] as f32));
                if v[2] > 0.0 {
                    l.aperture_range = Some((v[2] as f32, v[3] as f32));
                }
            }
        }
    }
}

/// Parse a GPS IFD.
pub fn read_gps(g: &Ifd) -> Option<GpsInfo> {
    let dms = |tag: u16| -> Option<f64> {
        let v = g.get(tag)?.f64s();
        (v.len() >= 3).then(|| v[0] + v[1] / 60.0 + v[2] / 3600.0)
    };
    let mut info = GpsInfo::default();
    if let Some(lat) = dms(0x0002) {
        let r = g.string(0x0001).unwrap_or_default();
        info.latitude = Some(if r.starts_with('S') { -lat } else { lat });
    }
    if let Some(lon) = dms(0x0004) {
        let r = g.string(0x0003).unwrap_or_default();
        info.longitude = Some(if r.starts_with('W') { -lon } else { lon });
    }
    if let Some(alt) = g.f64(0x0006) {
        let below = g.get(0x0005).and_then(|e| e.u32(0)).unwrap_or(0) == 1;
        info.altitude = Some(if below { -alt } else { alt });
    }
    if let (Some(d), Some(t)) = (g.string(0x001D), g.get(0x0007)) {
        let t = t.f64s();
        if t.len() >= 3 {
            info.timestamp =
                Some(format!("{}T{:02}:{:02}:{:02}Z", d.replace(':', "-"), t[0] as u32, t[1] as u32, t[2] as u32));
        }
    }
    (info.latitude.is_some() || info.longitude.is_some()).then_some(info)
}

/// Convert an EXIF `YYYY:MM:DD HH:MM:SS` timestamp into ISO 8601.
pub fn normalize_datetime(s: &str, subsec: Option<&str>, offset: Option<&str>) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || !b[..4].iter().all(|c| c.is_ascii_digit()) || s.starts_with("0000") {
        return None;
    }
    let mut out = format!("{}-{}-{}T{}", &s[0..4], &s[5..7], &s[8..10], &s[11..19]);
    if let Some(ss) = subsec.map(str::trim).filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit())) {
        out.push('.');
        out.push_str(ss);
    }
    if let Some(o) = offset.map(str::trim).filter(|v| v.len() == 6 && (v.starts_with('+') || v.starts_with('-'))) {
        out.push_str(o);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime() {
        assert_eq!(
            normalize_datetime("2021:03:04 05:06:07", Some("12"), Some("+02:00")).unwrap(),
            "2021-03-04T05:06:07.12+02:00"
        );
        assert!(normalize_datetime("0000:00:00 00:00:00", None, None).is_none());
        assert!(normalize_datetime("bad", None, None).is_none());
    }
}
