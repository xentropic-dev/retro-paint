//! The raylib window. Thin: the decisions (which frame, which screen, which
//! window size) live in the pure `view` module.

use std::ffi::{c_char, c_int};

use raylib::prelude::*;

use rp_doc::{FrameMsg, Layout};

use crate::crt::{CRT_FS, CrtParams, tube_rect};
use crate::view::{Change, DosState, StopSignal, idle_frame, window_size};
use crate::{FrameReceiver, IndexedScreen, sample};

/// Opens the DOS window on the calling thread and draws the newest frame
/// from `rx` through the CRT shader until `stop` fires or the window is
/// closed. Closing the DOS window only ends the view; the editor keeps
/// running and its frames are dropped.
///
/// Before the first frame it shows an empty canvas in the mobile layout
/// ([`idle_frame`]). With `RP_DOS_DEMO=mobile|desktop` set, it ignores `rx`
/// and shows the sample screen instead (see [`run_crt_demo`]).
pub fn run_window(rx: FrameReceiver, stop: StopSignal) {
    let initial = match std::env::var("RP_DOS_DEMO").as_deref() {
        Ok("mobile") => return run(sample::sample_frame(Layout::Mobile), None, &stop),
        Ok("desktop") => return run(sample::sample_frame(Layout::Desktop), None, &stop),
        _ => idle_frame(Layout::Mobile),
    };
    run(initial, Some(&rx), &stop);
}

/// Opens the DOS window showing the composed sample screen for `layout`
/// through the CRT shader, until it is closed. A visual check for the shader
/// and tube geometry.
pub fn run_crt_demo(layout: Layout) {
    run(sample::sample_frame(layout), None, &StopSignal::new());
}

/// Creates a point-filtered screen texture the size of `screen`.
fn screen_texture(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    screen: &IndexedScreen,
) -> Texture2D {
    let size = screen.size();
    let blank = Image::gen_image_color(size.width as i32, size.height as i32, Color::BLACK);
    let texture = rl
        .load_texture_from_image(thread, &blank)
        .expect("create DOS screen texture");
    texture.set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_POINT);
    texture
}

/// Uploads `screen` into `texture` (same size), reusing `rgba` as scratch.
fn upload(texture: &mut Texture2D, screen: &IndexedScreen, rgba: &mut Vec<u8>) {
    screen.write_rgba(rgba);
    texture.update_texture(rgba).expect("upload DOS screen");
}

fn run(initial: FrameMsg, rx: Option<&FrameReceiver>, stop: &StopSignal) {
    let mut state = DosState::new(initial);
    let (ww, wh) = window_size(state.layout());
    let (mut rl, thread) = raylib::init()
        .size(ww, wh)
        .title(crate::APP_ID)
        .resizable()
        .build();
    rl.set_target_fps(60);
    // The DOS window takes no input: Escape must not close it.
    rl.set_exit_key(None);

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
    shader.set_shader_value(curv, params.curvature);
    shader.set_shader_value(scan, params.scanline_intensity);
    shader.set_shader_value(flick, params.flicker);
    shader.set_shader_value(phos, Vector3::from(params.phosphor));

    // The composed screen is the offscreen texture: one per layout,
    // updated in place for each new frame.
    let mut rgba = Vec::new();
    let mut texture = screen_texture(&mut rl, &thread, state.screen());
    upload(&mut texture, state.screen(), &mut rgba);
    let mut size = state.screen().size();
    shader.set_shader_value(res, Vector2::new(size.width as f32, size.height as f32));

    while !stop.is_stopped() && !rl.window_should_close() {
        match state.accept(rx.and_then(FrameReceiver::try_recv)) {
            Change::None => {}
            Change::Screen => upload(&mut texture, state.screen(), &mut rgba),
            Change::Layout => {
                let (w, h) = window_size(state.layout());
                rl.set_window_size(w, h);
                texture = screen_texture(&mut rl, &thread, state.screen());
                upload(&mut texture, state.screen(), &mut rgba);
                size = state.screen().size();
                shader.set_shader_value(res, Vector2::new(size.width as f32, size.height as f32));
            }
        }
        shader.set_shader_value(time, rl.get_time() as f32);
        let tube = tube_rect(
            (rl.get_screen_width(), rl.get_screen_height()),
            state.layout(),
        );
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::BLACK);
        let mut s = d.begin_shader_mode(&mut shader);
        s.draw_texture_pro(
            &texture,
            Rectangle::new(0.0, 0.0, size.width as f32, size.height as f32),
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
