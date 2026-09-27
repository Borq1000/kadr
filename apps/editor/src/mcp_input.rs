//! Pointer input at window coordinates for MCP (`click_at`, `drag_at`).
//! Slint's own UI tools only act on element handles; these send the same
//! real `WindowEvent`s a mouse would, at any logical point of the window.

use kadr_mcp_bridge::BridgeError;
use serde_json::Value;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::LogicalPosition;

/// Moves between press and release of a drag unless `steps` says otherwise.
const DEFAULT_DRAG_STEPS: u64 = 12;
const MAX_DRAG_STEPS: u64 = 200;

fn bad(msg: impl Into<String>) -> BridgeError {
    BridgeError::new("bad_params", msg)
}

fn button(p: &Value) -> Result<PointerEventButton, BridgeError> {
    match p.get("button").and_then(Value::as_str) {
        None | Some("left") => Ok(PointerEventButton::Left),
        Some("right") => Ok(PointerEventButton::Right),
        Some("middle") => Ok(PointerEventButton::Middle),
        Some(other) => Err(bad(format!("`button` must be left|right|middle, got {other:?}"))),
    }
}

/// `{x, y}` in logical window pixels, inside a window of `size`.
fn point(v: Option<&Value>, what: &str, size: (f32, f32)) -> Result<LogicalPosition, BridgeError> {
    let coord = |k: &str| v.and_then(|v| v.get(k)).and_then(Value::as_f64);
    let (Some(x), Some(y)) = (coord("x"), coord("y")) else { return Err(bad(format!("`{what}` needs numbers x and y (logical window pixels)"))) };
    let (x, y) = (x as f32, y as f32);
    if !(0.0..size.0).contains(&x) || !(0.0..size.1).contains(&y) {
        return Err(bad(format!("{what} ({x}, {y}) is outside the window (0..{}, 0..{})", size.0, size.1)));
    }
    Ok(LogicalPosition::new(x, y))
}

fn click(at: LogicalPosition, button: PointerEventButton, double: bool) -> Vec<WindowEvent> {
    let mut out = vec![WindowEvent::PointerMoved { position: at }];
    for _ in 0..if double { 2 } else { 1 } {
        out.push(WindowEvent::PointerPressed { position: at, button });
        out.push(WindowEvent::PointerReleased { position: at, button });
    }
    out
}

fn drag(from: LogicalPosition, to: LogicalPosition, button: PointerEventButton, steps: u64) -> Vec<WindowEvent> {
    let mut out = vec![WindowEvent::PointerMoved { position: from }, WindowEvent::PointerPressed { position: from, button }];
    for i in 1..=steps {
        let f = i as f32 / steps as f32;
        out.push(WindowEvent::PointerMoved { position: LogicalPosition::new(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f) });
    }
    out.push(WindowEvent::PointerReleased { position: to, button });
    out
}

/// `click_at`: `{x, y, button?, double?}`.
pub fn click_events(p: &Value, size: (f32, f32)) -> Result<Vec<WindowEvent>, BridgeError> {
    let at = point(Some(p), "the point", size)?;
    Ok(click(at, button(p)?, p.get("double").and_then(Value::as_bool).unwrap_or(false)))
}

/// `drag_at`: `{from: {x, y}, to: {x, y}, button?, steps?}`.
pub fn drag_events(p: &Value, size: (f32, f32)) -> Result<Vec<WindowEvent>, BridgeError> {
    let from = point(p.get("from"), "from", size)?;
    let to = point(p.get("to"), "to", size)?;
    let steps = p.get("steps").and_then(Value::as_u64).unwrap_or(DEFAULT_DRAG_STEPS).clamp(1, MAX_DRAG_STEPS);
    Ok(drag(from, to, button(p)?, steps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WINDOW: (f32, f32) = (1600.0, 900.0);

    fn pos(e: &WindowEvent) -> (f32, f32) {
        match e {
            WindowEvent::PointerMoved { position } | WindowEvent::PointerPressed { position, .. } | WindowEvent::PointerReleased { position, .. } => (position.x, position.y),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn click_moves_there_then_presses_and_releases() {
        let ev = click_events(&json!({"x": 100, "y": 50.5, "button": "right"}), WINDOW).unwrap();
        assert_eq!(ev.len(), 3);
        assert!(matches!(ev[0], WindowEvent::PointerMoved { .. }));
        assert!(matches!(ev[1], WindowEvent::PointerPressed { button: PointerEventButton::Right, .. }));
        assert!(matches!(ev[2], WindowEvent::PointerReleased { button: PointerEventButton::Right, .. }));
        assert!(ev.iter().all(|e| pos(e) == (100.0, 50.5)));
        let double = click_events(&json!({"x": 1, "y": 1, "double": true}), WINDOW).unwrap();
        assert_eq!(double.iter().filter(|e| matches!(e, WindowEvent::PointerPressed { button: PointerEventButton::Left, .. })).count(), 2);
    }

    #[test]
    fn drag_presses_at_from_moves_in_steps_and_releases_at_to() {
        let ev = drag_events(&json!({"from": {"x": 10, "y": 20}, "to": {"x": 110, "y": 20}, "steps": 4}), WINDOW).unwrap();
        assert!(matches!(ev[1], WindowEvent::PointerPressed { .. }));
        assert_eq!(pos(&ev[1]), (10.0, 20.0));
        let xs: Vec<f32> = ev[2..ev.len() - 1].iter().map(|e| pos(e).0).collect();
        assert_eq!(xs, [35.0, 60.0, 85.0, 110.0]);
        assert!(matches!(ev.last(), Some(WindowEvent::PointerReleased { .. })));
        assert_eq!(pos(ev.last().unwrap()), (110.0, 20.0));
    }

    #[test]
    fn bad_points_and_buttons_are_rejected() {
        for p in [json!({"x": 10}), json!({"x": -1, "y": 5}), json!({"x": 1600, "y": 5}), json!({"x": "1", "y": 5}), json!({"x": 1, "y": 1, "button": "back"})] {
            assert_eq!(click_events(&p, WINDOW).unwrap_err().code, "bad_params", "{p}");
        }
        let e = drag_events(&json!({"from": {"x": 1, "y": 1}, "to": {"x": 5000, "y": 1}}), WINDOW).unwrap_err();
        assert!(e.message.contains("outside the window"), "{}", e.message);
    }
}
