use bevy::prelude::*;

#[derive(Component, Debug)]
pub struct Edge {
    pub from: Entity,
    pub to: Entity,
}
