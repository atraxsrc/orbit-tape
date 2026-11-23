//! A bell icon drawn on a canvas (the icon font has none), with an optional
//! unread badge in the top-right corner.

use iced::widget::canvas::{self, Canvas, Path, Stroke, Text};
use iced::{Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

struct Bell {
    unread: usize,
}

impl<Message> canvas::Program<Message> for Bell {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let palette = theme.extended_palette();
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let color = palette.background.base.text;

        // Bell body: a dome with flared sides, centred in the frame.
        let center = frame.center();
        let (cx, cy) = (center.x, center.y);
        let body = Path::new(|path| {
            path.move_to(Point::new(cx - 5.0, cy + 3.5));
            path.line_to(Point::new(cx - 4.0, cy + 2.0));
            path.line_to(Point::new(cx - 4.0, cy - 1.5));
            path.quadratic_curve_to(Point::new(cx - 4.0, cy - 5.5), Point::new(cx, cy - 5.5));
            path.quadratic_curve_to(
                Point::new(cx + 4.0, cy - 5.5),
                Point::new(cx + 4.0, cy - 1.5),
            );
            path.line_to(Point::new(cx + 4.0, cy + 2.0));
            path.line_to(Point::new(cx + 5.0, cy + 3.5));
            path.close();
        });
        frame.stroke(&body, Stroke::default().with_color(color).with_width(1.2));
        let clapper = Path::circle(Point::new(cx, cy + 5.0), 1.3);
        frame.fill(&clapper, color);

        if self.unread > 0 {
            let label = if self.unread > 99 {
                "99+".to_owned()
            } else {
                self.unread.to_string()
            };
            let width = 5.0 + 5.0 * label.len() as f32;
            let badge = Rectangle::new(
                Point::new(bounds.width - width, 0.0),
                Size::new(width, 10.0),
            );
            frame.fill(
                &Path::rounded_rectangle(badge.position(), badge.size(), 5.0.into()),
                palette.danger.base.color,
            );
            frame.fill_text(Text {
                content: label,
                position: badge.center(),
                color: palette.danger.base.text,
                size: 8.0.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                font: crate::style::AZERET_MONO,
                ..Text::default()
            });
        }

        vec![frame.into_geometry()]
    }
}

/// A 24 x 18 bell icon with an unread badge when `unread > 0`.
pub fn bell<'a, Message: 'a>(unread: usize) -> Element<'a, Message> {
    Canvas::new(Bell { unread })
        .width(Length::Fixed(24.0))
        .height(Length::Fixed(18.0))
        .into()
}
