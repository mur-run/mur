//! Pure window-placement geometry for the desktop pet and its chat panel.
//! Deliberately free of Tauri types so it unit-tests without the GUI stack.

/// A screen rectangle in PHYSICAL pixels, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// Horizontal gap between the pet and its panel, physical px.
const PANEL_GAP: i32 = 8;

/// Place a `panel`-sized window adjacent to `pet`, preferring the LEFT side.
/// Flips to the right of the pet if the left placement would start before the
/// monitor's left edge. The result is always clamped fully inside `mon`.
pub fn anchor_panel(pet: Rect, panel: (i32, i32), mon: Rect) -> (i32, i32) {
    let (pw, ph) = panel;
    let left_x = pet.x - PANEL_GAP - pw;
    let right_x = pet.right() + PANEL_GAP;
    let x = if left_x >= mon.x { left_x } else { right_x };
    // Align the panel's top with the pet's top, then clamp.
    clamp_into((x, pet.y), (pw, ph), mon)
}

/// Clamp a window of `size` at `pos` so it stays fully inside `mon`.
/// If the window is larger than the monitor, it pins to the monitor origin.
pub fn clamp_into(pos: (i32, i32), size: (i32, i32), mon: Rect) -> (i32, i32) {
    let (w, h) = size;
    let max_x = (mon.right() - w).max(mon.x);
    let max_y = (mon.bottom() - h).max(mon.y);
    (pos.0.clamp(mon.x, max_x), pos.1.clamp(mon.y, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect {
        x: 0,
        y: 0,
        w: 1440,
        h: 900,
    };

    #[test]
    fn anchors_to_left_of_pet_when_room() {
        // pet at x=800; panel 380 wide fits to the left.
        let pet = Rect {
            x: 800,
            y: 100,
            w: 300,
            h: 260,
        };
        let (x, y) = anchor_panel(pet, (380, 520), MON);
        assert_eq!(x, 800 - 8 - 380); // 412
        assert_eq!(y, 100);
    }

    #[test]
    fn flips_right_when_pet_near_left_edge() {
        // pet hugging the left edge: no room on the left, open to the right.
        let pet = Rect {
            x: 10,
            y: 100,
            w: 300,
            h: 260,
        };
        let (x, _) = anchor_panel(pet, (380, 520), MON);
        assert_eq!(x, 10 + 300 + 8); // 318
    }

    #[test]
    fn clamps_y_so_panel_bottom_stays_on_screen() {
        // pet low on screen: panel top would push the 520-tall panel off bottom.
        let pet = Rect {
            x: 800,
            y: 700,
            w: 300,
            h: 260,
        };
        let (_, y) = anchor_panel(pet, (380, 520), MON);
        assert_eq!(y, 900 - 520); // 380
    }

    #[test]
    fn clamp_pulls_offscreen_window_in() {
        // bottom-right overflow.
        assert_eq!(clamp_into((1400, 880), (300, 260), MON), (1140, 640));
        // negative origin.
        assert_eq!(clamp_into((-50, -30), (300, 260), MON), (0, 0));
        // already in-bounds is unchanged.
        assert_eq!(clamp_into((100, 100), (300, 260), MON), (100, 100));
    }

    #[test]
    fn window_larger_than_monitor_pins_to_origin() {
        let tiny = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 150,
        };
        assert_eq!(clamp_into((50, 50), (300, 260), tiny), (0, 0));
    }

    #[test]
    fn anchors_on_a_secondary_monitor_origin() {
        let mon = Rect { x: 1920, y: 0, w: 1440, h: 900 };
        let pet = Rect { x: 2000, y: 100, w: 300, h: 260 };
        let (x, y) = anchor_panel(pet, (380, 520), mon);
        assert_eq!(x, 2000 + 300 + 8); // 2308, opens right (no room on the left within this monitor)
        assert_eq!(y, 100);
    }

    #[test]
    fn clamps_within_secondary_monitor_bounds() {
        let mon = Rect { x: 1920, y: 0, w: 1440, h: 900 };
        assert_eq!(clamp_into((1900, 50), (300, 260), mon), (1920, 50));
    }
}
