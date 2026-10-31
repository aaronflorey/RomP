use slint::platform::WindowEvent;
use slint::winit_030::winit::window::WindowId;
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::time::Duration;

type Target = Box<dyn Fn(Option<WindowId>) -> bool>;

#[derive(Clone, Copy, PartialEq)]
struct ScaleState {
    native: f32,
    preferred: f32,
    effective: f32,
    physical: slint::PhysicalSize,
}

thread_local! {
    static FACTOR: Cell<f32> = const { Cell::new(1.0) };
    static GAME_MODE: Cell<bool> = const { Cell::new(false) };
    static TARGETS: RefCell<Vec<Target>> = RefCell::default();
}

pub fn set_game_mode(game_mode: bool) {
    GAME_MODE.set(game_mode);
    refresh(None);
}

pub fn set_factor(factor: f32) {
    FACTOR.set(factor);
    refresh(None);
}

pub fn track<C: ComponentHandle + 'static>(component: &C) {
    let last = Cell::new(None);
    apply(component.window(), &last);
    let weak = component.as_weak();
    TARGETS.with_borrow_mut(|targets| {
        targets.push(Box::new(move |only| {
            let Some(component) = weak.upgrade() else {
                return false;
            };
            let window = component.window();
            if only.is_none() || crate::mouse::window_id(window) == only {
                apply(window, &last);
            }
            true
        }));
    });
}

pub fn window_event() {
    refresh(None);
}

pub fn system_changed(id: WindowId) {
    slint::Timer::single_shot(Duration::ZERO, move || refresh(Some(id)));
}

fn refresh(only: Option<WindowId>) {
    let targets = TARGETS.take();
    let mut alive: Vec<Target> = targets.into_iter().filter(|t| t(only)).collect();
    TARGETS.with_borrow_mut(|targets| {
        alive.append(targets);
        *targets = alive;
    });
}

fn apply(window: &slint::Window, last: &Cell<Option<ScaleState>>) {
    let Some((id, system)) = window.with_winit_window(|w| (w.id(), w.scale_factor() as f32)) else {
        return;
    };
    let scale = effective_factor(system, FACTOR.get(), GAME_MODE.get());
    let physical = window.size();
    let state = ScaleState {
        native: system,
        preferred: FACTOR.get(),
        effective: scale,
        physical,
    };
    if last.replace(Some(state)) != Some(state) {
        tracing::info!(
            window = ?id,
            game_mode = GAME_MODE.get(),
            native = system,
            preferred = FACTOR.get(),
            effective = scale,
            physical = ?physical,
            logical = ?physical.to_logical(scale),
            "UI scale"
        );
    }
    if (window.scale_factor() - scale).abs() < f32::EPSILON {
        return;
    }
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: scale,
    });
    window.dispatch_event(WindowEvent::Resized {
        size: physical.to_logical(scale),
    });
    window.request_redraw();
}

fn effective_factor(system: f32, preference: f32, game_mode: bool) -> f32 {
    // Gamescope's X11 display can inherit a desktop DPI that is too large for its game viewport.
    if game_mode {
        preference
    } else {
        system * preference
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_mode_uses_the_chosen_scale_without_multiplying_desktop_dpi() {
        assert_eq!(effective_factor(3.0, 1.0, true), 1.0);
        assert_eq!(effective_factor(3.0, 1.5, true), 1.5);
        assert_eq!(effective_factor(2.0, 2.0, true), 2.0);
        assert_eq!(effective_factor(3.0, 1.0, false), 3.0);
        assert_eq!(effective_factor(2.0, 1.5, false), 3.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires X11: xvfb-run -a env WINIT_X11_SCALE_FACTOR=3 XDG_CURRENT_DESKTOP=KDE GAMESCOPE_WAYLAND_DISPLAY=gamescope-0 cargo test -p romp-app native_scale_lifecycle -- --ignored"]
    fn native_scale_lifecycle() {
        use slint::winit_030::winit::platform::x11::EventLoopBuilderExtX11;
        use slint::winit_030::{winit, CustomApplicationHandler, EventResult};
        use std::rc::Rc;

        struct ChangeDuringEvent(Rc<Cell<Option<f32>>>);
        impl CustomApplicationHandler for ChangeDuringEvent {
            fn window_event(
                &mut self,
                event_loop: &winit::event_loop::ActiveEventLoop,
                id: WindowId,
                native: Option<&winit::window::Window>,
                window: Option<&slint::Window>,
                event: &winit::event::WindowEvent,
            ) -> EventResult {
                let result =
                    crate::mouse::RawMouse.window_event(event_loop, id, native, window, event);
                if let Some(factor) = self.0.take() {
                    // A settings callback runs while Slint's native window is borrowed too.
                    set_factor(factor);
                }
                result
            }
        }

        slint::slint! {
            export component ScaleTestWindow inherits Window {
                preferred-width: 600px;
                preferred-height: 360px;
                out property <float> pointer-x: area.mouse-x / 1px;
                area := TouchArea {
                    width: 100%;
                    height: 100%;
                }
            }
        }

        set_game_mode(crate::gaming_mode::active());
        let mut builder = slint::winit_030::winit::event_loop::EventLoop::with_user_event();
        builder.with_x11().with_any_thread(true);
        let pending = Rc::new(Cell::new(None));
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .with_winit_event_loop_builder(builder)
            .with_winit_custom_application_handler(ChangeDuringEvent(pending.clone()))
            .select()
            .unwrap();
        let ui = ScaleTestWindow::new().unwrap();
        let play = ScaleTestWindow::new().unwrap();
        for window in [&ui, &play] {
            window.window().set_size(slint::PhysicalSize::new(600, 360));
            window.show().unwrap();
            track(window);
        }
        slint::spawn_local(async move {
            let native = ui.window().winit_window().await.unwrap();
            let other = play.window().winit_window().await.unwrap();
            assert_eq!(native.scale_factor(), 3.0);
            assert_eq!(
                ui.window().scale_factor(),
                1.0,
                "Gamescope's display must survive the KDE label"
            );
            assert_eq!(play.window().scale_factor(), 1.0);
            let physical = ui.window().size();
            pending.set(Some(1.5));
            native.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(0, 0));
            other.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(700, 0));
            native.request_redraw();
            slint::Timer::single_shot(Duration::from_millis(100), move || {
                assert_eq!(ui.window().scale_factor(), 1.5);
                assert_eq!(play.window().scale_factor(), 1.5);
                assert_eq!(ui.window().size(), physical);
                // Reproduce a backend DPI update; the next native event must restore the preference.
                ui.window()
                    .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 3.0 });
                system_changed(native.id());
                ui.window().set_size(slint::PhysicalSize::new(900, 540));
                native
                    .set_cursor_position(slint::winit_030::winit::dpi::PhysicalPosition::new(
                        150, 120,
                    ))
                    .unwrap();
                slint::Timer::single_shot(Duration::from_millis(100), move || {
                    assert_eq!(ui.window().scale_factor(), 1.5);
                    assert_eq!(ui.window().size(), slint::PhysicalSize::new(900, 540));
                    assert_eq!(
                        ui.get_pointer_x(),
                        100.0,
                        "native pointer positions use the effective scale"
                    );
                    set_factor(1.0);
                    assert_eq!(ui.window().scale_factor(), 1.0);
                    assert_eq!(play.window().scale_factor(), 1.0);
                    set_game_mode(false);
                    assert_eq!(
                        ui.window().scale_factor(),
                        3.0,
                        "desktop DPI remains intact"
                    );
                    ui.hide().unwrap();
                    play.hide().unwrap();
                    slint::quit_event_loop().unwrap();
                });
            });
        })
        .unwrap();
        slint::run_event_loop().unwrap();
    }
}
