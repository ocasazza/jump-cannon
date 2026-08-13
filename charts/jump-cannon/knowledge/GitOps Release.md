---
doctype: runbook
area: operations
audience: [operator, agent]
status: current
tags: [jump-cannon, gitops, flux]
---

# GitOps Release

The release sequence is source commit, Hydra image and chart builds, secured
artifact publication, Flux reconciliation, and live workload verification.
Each step needs its own evidence; a source build does not prove the cluster
updated.

There is no consumer chart-lock step. The packaged chart keeps version 0.1.0
but `appVersion` carries the source revision, so every source build changes
the tarball bytes at the stable object key
`gs://it-ops-nixstation-k8s-artifacts/charts/jump-cannon-0.1.0.tgz`. The
consumer environment follows it with a Flux Bucket source + HelmRelease: the
Bucket source sees a new artifact revision and the HelmRelease upgrades on
its own, and the Deployment's `chart.appVersion` pod annotation rolls the
pods onto the new `latest` image even when the chart templates did not
change.

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

## Consumer rollout requires the Revision strategy

The chart version is pinned `0.1.0` and republished in place, so the
consumer HelmRelease (envoy-ai-gateway
`lib/cluster-manifests/components/platforms/jump-cannon.nix`) must set
`reconcileStrategy: Revision`. Under the default `ChartVersion` strategy a
republished tarball has the same version and helm-controller never upgrades:
Hydra, GAR, and the chart cache all look green while the workload stays on
the old build. Verify with
`kubectl -n flux-system get helmrelease jump-cannon -o jsonpath='{.status.history[0].appVersion}'`
against the expected source sha.

## Hydra access without the sysmgr ssh key

`sudo -u hydra psql` on pdx-nxnx-lv01 needs the sysmgr key, which is not on
every machine. The HTTP API is the portable fallback: log in at
`http://pdx-nxnx-lv01.schrodinger.com:8080/hydra/login` as `hydra-ro`
(password in Secret `holmes/hydra-robot`, readable with the admin
kubeconfig), keep the session cookie, then query
`/jobset/<project>/<jobset>/evals` and `/build/<id>` with
`Accept: application/json`. Build status 0 is success, 1 failed, 2
dependency-failed.
