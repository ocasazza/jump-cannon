//! [`Binding`] = (trigger, action, sensitivity). [`BindingSet`] is
//! the serializable collection consumers persist alongside the rest
//! of their settings.

use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::event::Event;
use crate::raw::{PointerButtonSet, RawInput};
use crate::sensitivity::Sensitivity;
use crate::trigger::{Output, Trigger};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound = "A: Action")]
pub struct Binding<A: Action> {
    pub trigger: Trigger,
    pub action: A,
    #[serde(default)]
    pub sensitivity: Sensitivity,
}

impl<A: Action> Binding<A> {
    pub const fn new(trigger: Trigger, action: A) -> Self {
        Self {
            trigger,
            action,
            sensitivity: Sensitivity {
                gain: 1.0,
                curve: crate::sensitivity::Curve::Linear,
            },
        }
    }

    pub const fn with_sensitivity(mut self, s: Sensitivity) -> Self {
        self.sensitivity = s;
        self
    }

    /// Returns `Some(event)` if this binding fires this frame.
    /// `active_drags` lets [`Trigger::PointerDrag`] keep emitting
    /// after the initial press edge, so long as the button stays
    /// held.
    pub fn evaluate(
        &self,
        raw: &RawInput,
        active_drags: &PointerButtonSet,
    ) -> Option<Event<A>> {
        match self.trigger {
            Trigger::KeyPress { key, mods } => {
                if mods.matches(raw.modifiers) && raw.keys_pressed.contains(&key) {
                    debug_assert_eq!(self.trigger.output(), Output::Pulse);
                    Some(Event::Pulse(self.action.clone()))
                } else {
                    None
                }
            }
            Trigger::KeyHeld { key, mods } => {
                if mods.matches(raw.modifiers) && raw.keys_held.contains(&key) {
                    let v = self.sensitivity.apply(raw.dt);
                    if v == 0.0 {
                        None
                    } else {
                        Some(Event::Axis1(self.action.clone(), v))
                    }
                } else {
                    None
                }
            }
            Trigger::PointerPress { button, mods } => {
                if mods.matches(raw.modifiers) && raw.pointer_buttons_pressed.contains(button) {
                    Some(Event::Pulse(self.action.clone()))
                } else {
                    None
                }
            }
            Trigger::PointerDrag { button, mods } => {
                let pressed_now = raw.pointer_buttons_pressed.contains(button);
                let held_carry = active_drags.contains(button);
                if !(pressed_now || held_carry) {
                    return None;
                }
                if !mods.matches(raw.modifiers) {
                    return None;
                }
                let dx = self.sensitivity.apply(raw.pointer_delta[0]);
                let dy = self.sensitivity.apply(raw.pointer_delta[1]);
                if dx == 0.0 && dy == 0.0 {
                    return None;
                }
                Some(Event::Axis2(self.action.clone(), [dx, dy]))
            }
            Trigger::Wheel { mods } => {
                if !mods.matches(raw.modifiers) {
                    return None;
                }
                let v = self.sensitivity.apply(raw.wheel_delta);
                if v == 0.0 {
                    None
                } else {
                    Some(Event::Axis1(self.action.clone(), v))
                }
            }
            Trigger::Pinch => {
                // egui hands us a multiplicative factor — center on 1.0
                // and switch to log-space so curves operate on the
                // signed exponent rather than a number that's always >0.
                if raw.pinch_delta == 0.0 || raw.pinch_delta == 1.0 {
                    return None;
                }
                let log = raw.pinch_delta.ln();
                let v = self.sensitivity.apply(log);
                if v == 0.0 {
                    None
                } else {
                    Some(Event::Axis1(self.action.clone(), v))
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound = "A: Action")]
pub struct BindingSet<A: Action> {
    bindings: Vec<Binding<A>>,
}

impl<A: Action> Default for BindingSet<A> {
    fn default() -> Self {
        Self { bindings: Vec::new() }
    }
}

impl<A: Action> BindingSet<A> {
    pub fn new() -> Self {
        Self::default()
    }


    pub fn push(&mut self, b: Binding<A>) -> &mut Self {
        self.bindings.push(b);
        self
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Binding<A>> {
        self.bindings.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Binding<A>> {
        self.bindings.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Drop every binding pointing at `action`. Used by the rebinding
    /// UI before installing a replacement.
    pub fn clear_action(&mut self, action: &A) {
        self.bindings.retain(|b| &b.action != action);
    }
}
impl<A: Action> FromIterator<Binding<A>> for BindingSet<A> {
    fn from_iter<I: IntoIterator<Item = Binding<A>>>(it: I) -> Self {
        Self {
            bindings: it.into_iter().collect(),
        }
    }
}
