//! Predecoded, fixed-canvas recording indicators. No decoding occurs on timer ticks.
use image::AnimationDecoder;
use std::{io::Cursor, time::Duration};

pub struct Frame {
    pub image: slint::Image,
    #[cfg(windows)]
    pub tray: tray_icon::Icon,
    #[cfg(windows)]
    native: NativeIcon,
}

impl Frame {
    fn new(pixels: image::RgbaImage) -> anyhow::Result<Self> {
        let (width, height) = pixels.dimensions();
        Ok(Self {
            image: slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                pixels.as_raw(),
                width,
                height,
            )),
            #[cfg(windows)]
            tray: tray_icon::Icon::from_rgba(pixels.as_raw().clone(), width, height)?,
            #[cfg(windows)]
            native: NativeIcon::new(&pixels)?,
        })
    }

    #[cfg(windows)]
    pub fn update_taskbar(&self) {
        use windows::{core::w, Win32::UI::WindowsAndMessaging::*};
        // Only touch this process's main window, never another Echo instance.
        unsafe {
            let hwnd = FindWindowW(None, w!("Echo"));
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if hwnd.0 != 0 && pid == std::process::id() {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    windows::Win32::Foundation::WPARAM(1),
                    windows::Win32::Foundation::LPARAM(self.native.0 .0),
                );
            }
        }
    }
}

pub struct Icons {
    pub idle: Frame,
    pub frames: Vec<Frame>,
    ends: Vec<Duration>,
}

impl Icons {
    pub fn load() -> anyhow::Result<Self> {
        let idle = Frame::new(
            image::load_from_memory_with_format(
                include_bytes!("../eleventhecho.png"),
                image::ImageFormat::Png,
            )?
            .into_rgba8(),
        )?;
        let decoded = image::codecs::webp::WebPDecoder::new(Cursor::new(include_bytes!(
            "../assets/icons/transcribing.webp"
        )))?
        .into_frames()
        .collect_frames()?;
        anyhow::ensure!(!decoded.is_empty(), "Recording animation has no frames");
        let mut frames = Vec::new();
        let mut ends = Vec::new();
        let mut elapsed = Duration::ZERO;
        for frame in decoded {
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            elapsed += Duration::from_secs_f64(
                (f64::from(numerator) / f64::from(denominator) / 1000.0).max(0.05),
            );
            ends.push(elapsed);
            frames.push(Frame::new(frame.into_buffer())?);
        }
        Ok(Self { idle, frames, ends })
    }

    pub fn index(&self, elapsed: Duration) -> usize {
        let phase = elapsed.as_nanos() % self.ends.last().unwrap().as_nanos();
        self.ends
            .iter()
            .position(|end| phase < end.as_nanos())
            .unwrap()
    }
}

#[cfg(windows)]
struct NativeIcon(windows::Win32::UI::WindowsAndMessaging::HICON);

#[cfg(windows)]
impl NativeIcon {
    fn new(pixels: &image::RgbaImage) -> anyhow::Result<Self> {
        use windows::Win32::{Foundation::HINSTANCE, UI::WindowsAndMessaging::CreateIcon};
        let (width, height) = pixels.dimensions();
        let mut bgra = pixels.as_raw().clone();
        for pixel in bgra.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let mask = vec![0u8; (width.div_ceil(32) * 4 * height) as usize];
        // CreateIcon copies both buffers; the resulting handle lives with this frame.
        Ok(Self(unsafe {
            CreateIcon(
                HINSTANCE::default(),
                width as i32,
                height as i32,
                1,
                32,
                mask.as_ptr(),
                bgra.as_ptr(),
            )?
        }))
    }
}

#[cfg(windows)]
impl Drop for NativeIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplied_animation_has_three_fixed_canvas_frames_and_loops() {
        let icons = Icons::load().unwrap();
        assert_eq!(icons.frames.len(), 3);
        assert!(icons
            .frames
            .iter()
            .all(|f| f.image.size() == icons.frames[0].image.size()));
        assert_eq!(icons.index(Duration::ZERO), 0);
        assert_eq!(icons.index(icons.ends[0]), 1);
        assert_eq!(icons.index(icons.ends[1]), 2);
        assert_eq!(icons.index(*icons.ends.last().unwrap()), 0);
    }
}
