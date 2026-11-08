//! Live DDNet input sampling. The bindings are stored in the existing
//! validated client config, not in platform-dependent minifb key codes.
//! The gameplay loop samples once per frame and recognizes edge changes.
use minifb::{Key, MouseButton, Window};
use crate::server_browser::{KeyBindings, BIND_ACTIONS};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BindSnapshot {
    pub down: [bool; BIND_ACTIONS.len()],
}

impl BindSnapshot {
    pub fn capture(window: &Window, bindings: &KeyBindings) -> Self {
        Self::from_keys(&window.get_keys(), [
            window.get_mouse_down(MouseButton::Left),
            window.get_mouse_down(MouseButton::Right),
            window.get_mouse_down(MouseButton::Middle),
        ], bindings)
    }

    pub fn from_keys(keys: &[Key], mouse: [bool; 3], bindings: &KeyBindings) -> Self {
        let pressed = keys.iter().map(|key|format!("{key:?}")).collect::<Vec<_>>();
        let down = std::array::from_fn(|index| {
            match bindings.slots[index].as_str() {
                "MouseLeft" => mouse[0],
                "MouseRight" => mouse[1],
                "MouseMiddle" => mouse[2],
                name => pressed.iter().any(|key| key == name),
            }
        });
        Self { down }
    }

    pub fn held(&self, index: usize) -> bool {
        self.down.get(index).copied().unwrap_or(false)
    }

    pub fn pressed_since(&self, previous: Self, index: usize) -> bool {
        self.held(index) && !previous.held(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_mouse_map_to_configured_actions() {
        let mut binds=KeyBindings::default();
        let input=BindSnapshot::from_keys(&[Key::A,Key::W,Key::T], [true,false,false], &binds);
        assert!(input.held(0)); // left
        assert!(input.held(3)); // alternate jump
        assert!(input.held(4)); // fire
        assert!(input.held(7)); // chat
        assert!(!input.held(5)); // hook
        let previous=BindSnapshot::default();
        assert!(input.pressed_since(previous,7));
        assert!(!input.pressed_since(input,7));
        assert!(binds.set(0,"Q"));
        let input2=BindSnapshot::from_keys(&[Key::A,Key::Q], [false,true,false], &binds);
        assert!(input2.held(0));
        assert!(input2.held(5));
        assert!(!input2.held(4));
        let slots = BindSnapshot::from_keys(&[Key::Key1, Key::Key5], [false;3], &binds);
        assert!(slots.held(10));
        assert!(slots.held(14));
        assert!(!slots.held(12));
    }
    #[test]
    fn snapshot_indices_are_bounded() {
        assert!(!BindSnapshot::default().held(999));
    }
}
