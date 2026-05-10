//! Output of a fired binding — what consumers iterate over each frame.

use serde::{Deserialize, Serialize};

use crate::action::Action;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound = "A: Action")]
pub enum Event<A: Action> {
    /// Discrete edge — fired exactly once per press.
    Pulse(A),
    /// Continuous scalar — wheel, pinch, key-held axis time.
    Axis1(A, f32),
    /// Continuous 2-vector — pointer drag x/y, gamepad stick.
    Axis2(A, [f32; 2]),
}

impl<A: Action> Event<A> {
    pub fn action(&self) -> &A {
        match self {
            Event::Pulse(a) | Event::Axis1(a, _) | Event::Axis2(a, _) => a,
        }
    }
}
