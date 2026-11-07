//! The GUI client: a winit window rendered with wgpu + egui.

mod app;
pub mod client;
mod fonts;
mod icons;
mod keys;
mod term_view;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use egui::ViewportId;
use serde::{Deserialize, Serialize};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::paths;
use app::App;
use client::UserEvent;

#[derive(Serialize, Deserialize)]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
    /// Page ids collapsed in the sidebar.
    #[serde(default)]
    collapsed: Vec<u64>,
}

fn load_window_state() -> Option<WindowState> {
    serde_json::from_str(&std::fs::read_to_string(paths::window_file()).ok()?).ok()
}

fn save_window_state(window: &Window, collapsed: &std::collections::HashSet<u64>) {
    let size = window.inner_size();
    let pos = window.outer_position().unwrap_or_default();
    let state = WindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized(),
        collapsed: collapsed.iter().copied().collect(),
    };
    if let Ok(json) = serde_json::to_vec(&state) {
        let _ = paths::ensure_private_dir(&paths::data_dir());
        let _ = paths::write_private(&paths::window_file(), &json);
    }
}

struct Gfx {
    window: Arc<Window>,
    painter: egui_wgpu::winit::Painter,
    state: egui_winit::State,
}

struct Runner {
    app: App,
    gfx: Option<Gfx>,
    next_repaint: Option<Instant>,
    /// `SENNYA_DEBUG_FPS=1`: print frames per second and why they happen.
    debug: Option<(Instant, u32, Duration, Vec<Duration>)>,
}

impl Runner {
    fn window_attributes(&self) -> winit::window::WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title("sennya")
            .with_min_inner_size(LogicalSize::new(420.0, 260.0))
            .with_inner_size(LogicalSize::new(1180.0, 760.0));
        if let Some(s) = load_window_state() {
            // Extra windows open a little offset from the main one.
            let offset = if std::env::args().any(|a| a == "--window") { 48 } else { 0 };
            attrs = attrs
                .with_inner_size(PhysicalSize::new(s.width.max(420), s.height.max(260)))
                .with_position(PhysicalPosition::new(s.x + offset, s.y + offset))
                .with_maximized(s.maximized && offset == 0);
        }
        #[cfg(not(target_os = "macos"))]
        {
            attrs = attrs.with_window_icon(window_icon());
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{OptionAsAlt, WindowAttributesExtMacOS};
            let option = match self.app.config.terminal.option_as_alt {
                crate::config::OptionAsAlt::None => OptionAsAlt::None,
                crate::config::OptionAsAlt::Left => OptionAsAlt::OnlyLeft,
                crate::config::OptionAsAlt::Right => OptionAsAlt::OnlyRight,
                crate::config::OptionAsAlt::Both => OptionAsAlt::Both,
            };
            attrs = attrs
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
                .with_option_as_alt(option);
        }
        attrs
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Runner { app, gfx, next_repaint, debug } = self;
        let Some(gfx) = gfx.as_mut() else { return };
        let started = Instant::now();
        let raw = gfx.state.take_egui_input(&gfx.window);
        let ctx = app.ctx.clone();
        let output = ctx.run(raw, |ctx| app.frame(ctx));
        gfx.state.handle_platform_output(&gfx.window, output.platform_output);
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        gfx.painter.paint_and_update_textures(
            ViewportId::ROOT,
            output.pixels_per_point,
            app.clear_color(),
            &primitives,
            &output.textures_delta,
            Vec::new(),
        );
        let egui_delay = output.viewport_output.get(&ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(Duration::MAX);
        let delay = egui_delay.min(app.next_wakeup());
        let arrived = app.frame_arrived.take();
        if let Some((since, frames, worst, lat)) = debug {
            *frames += 1;
            *worst = (*worst).max(started.elapsed());
            if let Some(t) = arrived {
                lat.push(t.elapsed());
            }
            if since.elapsed() >= Duration::from_secs(1) {
                lat.sort();
                let (p50, max) = (lat.get(lat.len() / 2).copied().unwrap_or_default(), lat.last().copied().unwrap_or_default());
                let atlas = ctx.fonts(|f| f.font_image_size());
                eprintln!("fps {frames}  worst frame {worst:?}  frame arrived->presented p50 {p50:?} max {max:?}  egui_delay {egui_delay:?}  font atlas {atlas:?}");
                *since = Instant::now();
                *frames = 0;
                *worst = Duration::ZERO;
                lat.clear();
            }
        }
        *next_repaint = if delay.is_zero() {
            gfx.window.request_redraw();
            None
        } else if delay < Duration::from_secs(3600) {
            Some(Instant::now() + delay)
        } else {
            None
        };
        if app.quit {
            app.on_close();
            save_window_state(&gfx.window, &app.collapsed);
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<UserEvent> for Runner {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.next_repaint = None;
            if let Some(g) = &self.gfx {
                g.window.request_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let window = match event_loop.create_window(self.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                eprintln!("sennya: cannot create a window: {err}");
                event_loop.exit();
                return;
            }
        };
        let ctx = self.app.ctx.clone();
        // Allocate GPU memory in small blocks: a terminal draws little, and the default
        // (tuned for games) reserves large heaps up front.
        let base = egui_wgpu::WgpuSetupCreateNew::default();
        let describe = base.device_descriptor.clone();
        let setup = egui_wgpu::WgpuSetupCreateNew {
            power_preference: egui_wgpu::wgpu::PowerPreference::LowPower,
            device_descriptor: Arc::new(move |adapter| {
                let mut d = describe(adapter);
                d.memory_hints = egui_wgpu::wgpu::MemoryHints::MemoryUsage;
                d
            }),
            ..base
        };
        let config = egui_wgpu::WgpuConfiguration {
            // One queued frame: what you type reaches the screen on the next vsync.
            desired_maximum_frame_latency: Some(1),
            wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
            ..Default::default()
        };
        let options = egui_wgpu::RendererOptions { msaa_samples: 1, ..Default::default() };
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(ctx.clone(), config, false, options));
        if let Err(err) = pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone()))) {
            eprintln!("sennya: GPU initialisation failed: {err}");
            event_loop.exit();
            return;
        }
        let state = egui_winit::State::new(
            ctx,
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        self.app.window = Some(window.clone());
        window.request_redraw();
        self.gfx = Some(Gfx { window, painter, state });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        // (OutdatedDaemon and the rest are handled by the app.)
        if self.app.on_user_event(event) {
            if let Some(g) = &self.gfx {
                g.window.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        match &event {
            WindowEvent::CloseRequested => {
                self.app.on_close();
                save_window_state(&gfx.window, &self.app.collapsed);
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                    gfx.painter.on_window_resized(ViewportId::ROOT, w, h);
                }
            }
            WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } => {
                if let Some(redraw) = self.app.on_key(key) {
                    if redraw {
                        gfx.window.request_redraw();
                    }
                    return;
                }
            }
            WindowEvent::ModifiersChanged(m) => self.app.modifiers = m.state(),
            WindowEvent::DroppedFile(path) => {
                self.app.dropped.push(path.clone());
                gfx.window.request_redraw();
            }
            WindowEvent::HoveredFile(_) => {
                self.app.hovering_files = true;
                gfx.window.request_redraw();
            }
            WindowEvent::HoveredFileCancelled => {
                self.app.hovering_files = false;
                gfx.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                self.app.set_window_focus(*focused);
                gfx.window.request_redraw();
            }
            WindowEvent::Occluded(occluded) => {
                self.app.on_occluded(*occluded);
                if !occluded {
                    gfx.window.request_redraw();
                }
            }
            _ => {}
        }
        let response = gfx.state.on_window_event(&gfx.window, &event);
        if response.repaint {
            gfx.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Also reached through the macOS app menu's Quit; terminals keep running in the daemon.
        self.app.on_close();
        if let Some(g) = &self.gfx {
            save_window_state(&g.window, &self.app.collapsed);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.next_repaint {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// Polls the config file's mtime (one `stat` per second) and pings the UI on change.
fn watch_config(proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let path = paths::config_file();
        let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let mut last: Option<SystemTime> = mtime(&path);
        loop {
            thread::sleep(Duration::from_secs(1));
            let now = mtime(&path);
            if now != last {
                last = now;
                if proxy.send_event(UserEvent::ConfigChanged).is_err() {
                    return;
                }
            }
        }
    });
}

/// The app icon for the title bar and taskbar.
#[cfg(not(target_os = "macos"))]
fn window_icon() -> Option<winit::window::Icon> {
    let decoder = png::Decoder::new(std::io::Cursor::new(&include_bytes!("../../assets/icon-256.png")[..]));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    winit::window::Icon::from_rgba(rgba, info.width, info.height).ok()
}

pub fn run() -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    watch_config(proxy.clone());
    let ctx = egui::Context::default();
    let mut app = App::new(ctx, proxy);
    if let Some(state) = load_window_state() {
        app.collapsed = state.collapsed.into_iter().collect();
    }
    let debug = std::env::var_os("SENNYA_DEBUG_FPS").map(|_| (Instant::now(), 0, Duration::ZERO, Vec::new()));
    let mut runner = Runner { app, gfx: None, next_repaint: None, debug };
    event_loop.run_app(&mut runner)?;
    Ok(())
}
