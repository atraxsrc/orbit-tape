//! A thin vertical edge that resizes the panel beside it by dragging,
//! like the splits between panes. It reports how far the cursor moved
//! sideways since the last event; the owner applies and clamps the width.

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        layout::{Limits, Node},
        renderer::{Quad, Renderer as _, Style},
        widget::{Tree, tree},
    },
    mouse::{self, Cursor, Interaction},
};

/// Width of the grab area; the visible line is 1 px in its middle.
const GRAB_WIDTH: f32 = 6.0;

#[derive(Default)]
struct State {
    /// Cursor x at the last drag event, while dragging.
    drag_x: Option<f32>,
}

pub struct ResizeHandle<Message> {
    on_drag: fn(f32) -> Message,
}

/// `on_drag` gets the sideways move in pixels, positive to the right.
pub fn resize_handle<Message>(on_drag: fn(f32) -> Message) -> ResizeHandle<Message> {
    ResizeHandle { on_drag }
}

impl<Message> Widget<Message, Theme, Renderer> for ResizeHandle<Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(GRAB_WIDTH), Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        Node::new(limits.resolve(Length::Fixed(GRAB_WIDTH), Length::Fill, Size::ZERO))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let Event::Mouse(event) = event else {
            return;
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                if let Some(position) = cursor.position_over(layout.bounds()) {
                    state.drag_x = Some(position.x);
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            mouse::Event::CursorMoved { position } => {
                if let Some(last) = state.drag_x {
                    let dx = position.x - last;
                    if dx != 0.0 {
                        state.drag_x = Some(position.x);
                        shell.publish((self.on_drag)(dx));
                    }
                    shell.capture_event();
                } else if cursor.is_over(layout.bounds()) {
                    shell.request_redraw();
                }
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) if state.drag_x.is_some() => {
                state.drag_x = None;
                shell.capture_event();
                shell.request_redraw();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &Style,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let active = state.drag_x.is_some() || cursor.is_over(bounds);
        let palette = theme.extended_palette();
        let color = if active {
            palette.primary.base.color
        } else {
            palette.background.strong.color.scale_alpha(0.25)
        };
        let width = if active { 2.0 } else { 1.0 };
        renderer.fill_quad(
            Quad {
                bounds: Rectangle {
                    x: bounds.center_x() - width / 2.0,
                    width,
                    ..bounds
                },
                snap: true,
                ..Quad::default()
            },
            color,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.drag_x.is_some() || cursor.is_over(layout.bounds()) {
            Interaction::ResizingHorizontally
        } else {
            Interaction::default()
        }
    }
}

impl<'a, Message: 'a> From<ResizeHandle<Message>> for Element<'a, Message> {
    fn from(handle: ResizeHandle<Message>) -> Self {
        Self::new(handle)
    }
}
