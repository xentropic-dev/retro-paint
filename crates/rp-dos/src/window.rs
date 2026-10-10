//! The raylib window. Thin: all logic lives in the pure functions in `lib.rs`.

use std::ffi::{c_char, c_int};

use raylib::prelude::*;

use crate::{CGA, FrameReceiver, fit_integer_scale, indexed_to_rgba};

/// Width and height of the spike's indexed screen (mobile layout).
const SCREEN: (i32, i32) = (200, 320);

/// Opens the DOS window on the calling thread and draws frames until it is closed.
pub fn run_window(rx: FrameReceiver) {
    let (mut rl, thread) = raylib::init()
        .size(SCREEN.0 * 2, SCREEN.1 * 2)
        .title(crate::APP_ID)
        .resizable()
        .build();
    rl.set_target_fps(60);

    let blank = Image::gen_image_color(SCREEN.0, SCREEN.1, Color::BLACK);
    let mut texture = rl
        .load_texture_from_image(&thread, &blank)
        .expect("create DOS screen texture");
    texture.set_texture_filter(&thread, TextureFilter::TEXTURE_FILTER_POINT);
    let mut rgba = Vec::new();

    while !rl.window_should_close() {
        if let Some(frame) = rx.try_recv()
            && (frame.width as i32, frame.height as i32) == SCREEN
        {
            indexed_to_rgba(&frame.pixels, &CGA, &mut rgba);
            texture.update_texture(&rgba).expect("upload DOS frame");
        }
        let (x, y, w, h) =
            fit_integer_scale(SCREEN, (rl.get_screen_width(), rl.get_screen_height()));
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::BLACK);
        d.draw_texture_pro(
            &texture,
            Rectangle::new(0.0, 0.0, SCREEN.0 as f32, SCREEN.1 as f32),
            Rectangle::new(x as f32, y as f32, w as f32, h as f32),
            Vector2::zero(),
            0.0,
            Color::WHITE,
        );
    }
}

// raylib's InitWindow calls glfwDefaultWindowHints() right before creating the
// window, which clears any app_id hint we could set beforehand. The binary links
// with `--wrap=glfwDefaultWindowHints` (see build.rs) so this wrapper re-applies
// the Wayland app_id and X11 class after the reset.
const GLFW_X11_CLASS_NAME: c_int = 0x0002_4001;
const GLFW_X11_INSTANCE_NAME: c_int = 0x0002_4002;
const GLFW_WAYLAND_APP_ID: c_int = 0x0002_6001;

unsafe extern "C" {
    fn __real_glfwDefaultWindowHints();
    fn glfwWindowHintString(hint: c_int, value: *const c_char);
}

/// Linker wrapper for `glfwDefaultWindowHints`; see the comment above.
#[unsafe(no_mangle)]
pub extern "C" fn __wrap_glfwDefaultWindowHints() {
    let id = c"retro-paint-dos".as_ptr();
    // SAFETY: plain GLFW calls on the thread that is initializing the window;
    // `id` is a static NUL-terminated string that GLFW copies.
    unsafe {
        __real_glfwDefaultWindowHints();
        glfwWindowHintString(GLFW_WAYLAND_APP_ID, id);
        glfwWindowHintString(GLFW_X11_CLASS_NAME, id);
        glfwWindowHintString(GLFW_X11_INSTANCE_NAME, id);
    }
}
