//! The raylib window. Thin: all logic lives in the pure functions in `lib.rs`.

use std::ffi::{c_char, c_int};

use raylib::prelude::*;

use rp_doc::Layout;
use rp_palette_dither::CGA_RGB;

use crate::crt::{CRT_FS, CrtParams, tube_rect};
use crate::{FrameReceiver, ScrollState, compose, fit_integer_scale, indexed_to_rgba, sample};

/// Width and height of the spike's indexed screen (mobile layout).
const SCREEN: (i32, i32) = (200, 320);

/// Opens the DOS window on the calling thread and draws frames until it is closed.
///
/// With `RP_DOS_DEMO=mobile|desktop` set, it ignores `rx` and shows the
/// sample screen through the CRT shader instead (see [`run_crt_demo`]).
pub fn run_window(rx: FrameReceiver) {
    match std::env::var("RP_DOS_DEMO").as_deref() {
        Ok("mobile") => return run_crt_demo(Layout::Mobile),
        Ok("desktop") => return run_crt_demo(Layout::Desktop),
        _ => {}
    }
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
            indexed_to_rgba(&frame.pixels, &CGA_RGB, &mut rgba);
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

/// Opens the DOS window showing the composed sample screen for `layout`
/// through the CRT shader, until it is closed. A visual check for the shader
/// and tube geometry.
pub fn run_crt_demo(layout: Layout) {
    let size = layout.dos_screen();
    let (sw, sh) = (size.width as i32, size.height as i32);
    let window = match layout {
        Layout::Mobile => (450, 800),
        Layout::Desktop => (960, 540),
    };
    let (mut rl, thread) = raylib::init()
        .size(window.0, window.1)
        .title(crate::APP_ID)
        .resizable()
        .build();
    rl.set_target_fps(60);

    let screen = compose(
        &sample::sample_frame(layout),
        layout,
        &mut ScrollState::default(),
    );
    let image = Image::gen_image_color(sw, sh, Color::BLACK);
    let mut texture = rl
        .load_texture_from_image(&thread, &image)
        .expect("create DOS screen texture");
    texture.set_texture_filter(&thread, TextureFilter::TEXTURE_FILTER_POINT);
    texture
        .update_texture(&screen.to_rgba())
        .expect("upload DOS screen");

    let mut target = rl
        .load_render_texture(&thread, sw as u32, sh as u32)
        .expect("create DOS render texture");
    target
        .texture()
        .set_texture_filter(&thread, TextureFilter::TEXTURE_FILTER_POINT);

    let params = CrtParams::default();
    let mut shader = rl.load_shader_from_memory(&thread, None, Some(CRT_FS));
    let loc = |s: &Shader, name: &str| s.get_shader_location(name);
    let (res, curv, scan, time, flick, phos) = (
        loc(&shader, "resolution"),
        loc(&shader, "curvature"),
        loc(&shader, "scanlineIntensity"),
        loc(&shader, "time"),
        loc(&shader, "flicker"),
        loc(&shader, "phosphor"),
    );
    shader.set_shader_value(res, Vector2::new(sw as f32, sh as f32));
    shader.set_shader_value(curv, params.curvature);
    shader.set_shader_value(scan, params.scanline_intensity);
    shader.set_shader_value(flick, params.flicker);
    shader.set_shader_value(phos, Vector3::from(params.phosphor));

    while !rl.window_should_close() {
        {
            let mut t = rl.begin_texture_mode(&thread, &mut target);
            t.clear_background(Color::BLACK);
            t.draw_texture(&texture, 0, 0, Color::WHITE);
        }
        shader.set_shader_value(time, rl.get_time() as f32);
        let tube = tube_rect((rl.get_screen_width(), rl.get_screen_height()), layout);
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::BLACK);
        let mut s = d.begin_shader_mode(&mut shader);
        // Render textures are stored bottom-up: flip with a negative height.
        s.draw_texture_pro(
            target.texture(),
            Rectangle::new(0.0, 0.0, sw as f32, -(sh as f32)),
            Rectangle::new(tube.x as f32, tube.y as f32, tube.w as f32, tube.h as f32),
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
