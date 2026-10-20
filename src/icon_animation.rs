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
        use windows::Win32::UI::WindowsAndMessaging::*;
        // Only touch this process's main window, never another Echo instance.
        unsafe {
            let hwnd = main_window();
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

// Other Echo windows (including isolated inspection processes) can share the
// title. FindWindow returns only the first one and can suppress our animation.
#[cfg(windows)]
pub fn main_window() -> windows::Win32::Foundation::HWND {
    use windows::Win32::{
        Foundation::{BOOL, HWND, LPARAM},
        UI::WindowsAndMessaging::*,
    };
    unsafe extern "system" fn visit(hwnd: HWND, result: LPARAM) -> BOOL {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid == std::process::id() {
            let mut title = [0u16; 16];
            let count = unsafe { GetWindowTextW(hwnd, &mut title) };
            // A window procedure can return the full title length even when
            // WM_GETTEXT truncated it to our buffer (including Slint windows).
            // Never use that untrusted count as a slice boundary in this FFI
            // callback: a panic here cannot unwind and aborts the application.
            if is_main_window_title(&title, count) {
                unsafe {
                    *(result.0 as *mut HWND) = hwnd;
                }
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut result = HWND::default();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM((&mut result as *mut HWND) as isize));
    }
    result
}

#[cfg(windows)]
fn is_main_window_title(title: &[u16], count: i32) -> bool {
    count == 4 && title.get(..4) == Some(&[69, 99, 104, 111])
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
    #[cfg(windows)]
    #[test]
    fn window_title_accepts_only_echo_and_rejects_invalid_reported_lengths() {
        let title = [69, 99, 104, 111, 0];
        assert!(is_main_window_title(&title, 4));
        for count in [-1, 0, 3, 5, 22, i32::MAX] {
            assert!(!is_main_window_title(&title, count));
        }
        assert!(!is_main_window_title(&title[..3], 4));
        assert!(!is_main_window_title(&[69, 67, 72, 79], 4));
    }
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
