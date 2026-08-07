---
doctype: runbook
area: documentation
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, documentation]
---

# Maintaining This Map

`charts/jump-cannon/knowledge` is the canonical corpus. Every note must be
reachable from [[Start Here]], use exact wikilinks, state its audience and
status, and distinguish current behavior from planned work.

Helm synchronizes these files into the reserved `Jump Cannon/` vault folder.
Edit source control, not the deployed copy. Validate links through [[Testing]],
then deliver changes through [[Helm Deployment]] and [[Agent Workflow]].
