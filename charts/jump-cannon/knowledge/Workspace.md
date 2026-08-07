---
doctype: guide
area: product
audience: [user]
status: current
tags: [jump-cannon, frontend]
---

# Workspace

Graph and Nodes are the primary surfaces. Nodes combines a Flat/Tags navigator
on the left with the selected node's content/editor on the right. Inspector and
Document remain detachable dock views; Progress and Settings support repeated
exploration without leaving the workspace.

Use the Graph header's Fit action whenever the graph leaves the viewport. Open
notes through [[Nodes Search and Documents]], then use
[[Layouts Metrics and Filters]] to compare structure. The workbench stacks its
navigator above content when its panel becomes narrow. Panel placement persists
locally through panel-kit; the Nodes-workbench release adopts a new default
layout once, then persists subsequent changes. The toolbar always starts below
the panel header, its importer search-key strip scrolls horizontally instead of
consuming the editor, and the default tiling span reserves a half-width,
four-row Nodes surface. The navigator only stacks above content below a 480px
panel width. Layout migration upgrades the historical 1x2 Nodes tile to this
editor span while preserving any other user-selected tile size.
