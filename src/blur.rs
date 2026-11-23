//! Compositor blur behind the glass window, on Wayland.
//!
//! iced asks for blur through winit, which only speaks KDE's
//! `org_kde_kwin_blur`. COSMIC doesn't offer that; it offers the newer
//! `ext-background-effect-v1`. This binds that protocol on winit's own
//! Wayland connection and asks for each window to be blurred while its
//! background is see-through.
//!
//! The blur region is double-buffered, so a change shows on the window's
//! next frame. Setting it after the window's surface is gone is a protocol
//! error that would end the connection, so a window's entry is dropped as
//! soon as iced reports it closed, and nothing is sent for windows not in
//! the list.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use iced::window;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::{Backend, ObjectId},
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry::WlRegistry,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, Capability, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};

/// Covers any real window; the compositor clips the region to the surface.
const REGION_SIZE: i32 = 1 << 16;

static WINDOWS: LazyLock<Mutex<HashMap<window::Id, Blur>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Starts managing blur for a newly opened window and applies `on`.
///
/// Runs inside `window::run`, so on the thread that owns the window.
pub fn attach(id: window::Id, window: &dyn iced::window::Window, on: bool) {
    match Blur::new(window) {
        Ok(mut blur) => {
            blur.set(on);
            if let Ok(mut windows) = WINDOWS.lock() {
                windows.insert(id, blur);
            }
        }
        Err(err) => log::info!("Window blur unavailable: {err}"),
    }
}

/// Stops managing a window. Call once iced reports it closed.
pub fn detach(id: window::Id) {
    if let Ok(mut windows) = WINDOWS.lock()
        && let Some(blur) = windows.remove(&id)
    {
        // Allowed on an inert object; only set_blur_region would be an error.
        blur.effect.destroy();
        let _ = blur.connection.flush();
    }
}

/// Turns blur on or off for every open window.
pub fn set_all(on: bool) {
    if let Ok(mut windows) = WINDOWS.lock() {
        for blur in windows.values_mut() {
            blur.set(on);
        }
    }
}

struct Blur {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    compositor: WlCompositor,
    /// Kept bound so capability changes keep arriving.
    _manager: ExtBackgroundEffectManagerV1,
    effect: ExtBackgroundEffectSurfaceV1,
    /// What was last sent, so unchanged requests are skipped.
    sent: Option<bool>,
}

#[derive(Default)]
struct State {
    /// Whether the compositor can blur right now (its capabilities event).
    can_blur: bool,
}

impl Blur {
    fn new(window: &dyn iced::window::Window) -> Result<Self, String> {
        let display = window.display_handle().map_err(|err| err.to_string())?;
        let RawDisplayHandle::Wayland(display) = display.as_raw() else {
            return Err("not a Wayland window".to_owned());
        };
        let handle = window.window_handle().map_err(|err| err.to_string())?;
        let RawWindowHandle::Wayland(handle) = handle.as_raw() else {
            return Err("not a Wayland window".to_owned());
        };

        // SAFETY: the display belongs to winit and outlives every window, and
        // a foreign backend never disconnects it. Our requests go through our
        // own event queue.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, mut queue) =
            registry_queue_init::<State>(&connection).map_err(|err| err.to_string())?;
        let qh = queue.handle();

        let manager: ExtBackgroundEffectManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| "compositor has no ext_background_effect_manager_v1".to_owned())?;
        let compositor: WlCompositor = globals
            .bind(&qh, 1..=6, ())
            .map_err(|err| err.to_string())?;

        // SAFETY: the pointer is winit's live wl_surface for this window; the
        // proxy only borrows it and never destroys it.
        let surface_id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }
                .map_err(|_| "invalid surface".to_owned())?;
        let surface = WlSurface::from_id(&connection, surface_id)
            .map_err(|_| "invalid surface".to_owned())?;

        let effect = manager.get_background_effect(&surface, &qh, ());

        // Receive the capabilities sent on bind.
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|err| err.to_string())?;
        if !state.can_blur {
            log::info!("Compositor offers background effects but not blur right now");
        }

        Ok(Self {
            connection,
            queue,
            state,
            compositor,
            _manager: manager,
            effect,
            sent: None,
        })
    }

    fn set(&mut self, on: bool) {
        // Pick up capability changes; blur set while unsupported is ignored.
        let _ = self.queue.dispatch_pending(&mut self.state);
        if self.sent == Some(on) {
            return;
        }

        if on {
            let region = self.compositor.create_region(&self.queue.handle(), ());
            region.add(0, 0, REGION_SIZE, REGION_SIZE);
            self.effect.set_blur_region(Some(&region));
            region.destroy();
        } else {
            self.effect.set_blur_region(None);
        }

        if let Err(err) = self.connection.flush() {
            log::warn!("Failed to send window blur request: {err}");
        }
        self.sent = Some(on);
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            state.can_blur =
                matches!(flags, WEnum::Value(flags) if flags.contains(Capability::Blur));
        }
    }
}

wayland_client::delegate_noop!(State: ignore WlCompositor);
wayland_client::delegate_noop!(State: ignore WlRegion);
wayland_client::delegate_noop!(State: ignore ExtBackgroundEffectSurfaceV1);
