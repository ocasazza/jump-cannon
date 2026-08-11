---
doctype: runbook
area: operations
audience: [operator, agent]
status: current
tags: [jump-cannon, gitops, flux]
---

# GitOps Release

The release sequence is source commit, Hydra image and chart builds, secured
artifact publication, consumer chart lock update, rendered manifest review,
Flux reconciliation, and live workload verification. Each step needs its own
evidence; a source build does not prove the cluster updated.

Environment-owned resources include [[NetBird Access]] and GPU admission under
[[Kueue Scheduling]]. Confirm the deployed result in [[Observability]].

## Publication and verification truth

Hydra builds run on pdx-nxnx-lv01; the queue-runner copies outputs to the
local cache, and per-job `runcommand` hooks in `/etc/hydra/hydra.conf` push
images to GAR (`latest` plus `build-<id>` tags, via skopeo with the
lavender-image-writer key) and the chart tarball to the hydra-cache chart
URL. Build outcomes live in the Hydra postgres `builds` table
(`sudo -u hydra psql hydra` on the host).

Do not trust `gcloud artifacts docker images list --sort-by=~update_time` to
prove a push happened — its update times do not track tag activity and
pagination silently truncates. The source of truth is the registry's own
API: `curl -H "Authorization: Bearer $(gcloud auth print-access-token)"
https://us-central1-docker.pkg.dev/v2/it-ops-nixstation/lavender/<image>/tags/list`
and look for the expected `build-<id>` tag. Cross-check that before
declaring the pipeline dead; a green Hydra build plus a missing GAR tag is
the only real publish failure.

Manual bypass when Hydra genuinely cannot push: build the image via the
remote builder (`ssh://root@pdx-nxst-001.schrodinger.com` is in
`/etc/nix/machines` and reachable off-NetBird), stream the closure down,
and `crane push` the tarball to GAR `latest`.
