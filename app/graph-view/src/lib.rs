//! Jump Cannon's graph view: the WebGPU renderer and force layout
//! ([`render`]) and the canvas component that hosts it ([`GraphView`]).
//! Hosts mount a [`render::Scene`] with [`render::mount_canvas`].

pub mod render;

mod canvas;

pub use canvas::GraphView;

/// Base styling for [`GraphView`]: the canvas fills its panel and status
/// overlays sit over it. Expects panel-kit's theme variables.
pub const CSS: &str = "
.graph-wrap { position:relative; width:100%; height:100%; }
.graph-canvas { display:block; width:100%; height:100%; cursor:crosshair; }
.graph-render-status { position:absolute; z-index:1; inset:1.75rem .75rem .75rem;
  display:flex; flex-direction:column; align-items:center; justify-content:center;
  padding:1.25rem; border:1px solid var(--line2); border-radius:4px;
  background:color-mix(in srgb, var(--panel) 94%, transparent); text-align:center; }
.graph-render-status.initializing { color:var(--dim); font-size:.72rem; pointer-events:none; }
.graph-render-status.unavailable { border-color:var(--yellow); }
.graph-render-status h2 { margin:0 0 .45rem; color:var(--fg); font-size:.9rem; }
.graph-render-status p { max-width:36rem; margin:0; color:var(--dim); font-size:.72rem; line-height:1.55; }
";
