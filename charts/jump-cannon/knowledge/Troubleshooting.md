---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, troubleshooting, dns]
---

# Troubleshooting

Check the layers in order: stable service-name resolution, Gateway or NetBird
routing, graph-api health, asset delivery, graph counts and IDs, browser console,
WebGPU canvas, compute admission, then dashboard ingestion.

For search or filtering, fetch `GET /graph/schema` and confirm its
`graph_revision`, source identity, declared query keys, and facetable fields.
An importer that omits a required schema or emits a missing, duplicate,
undeclared, or mistyped search document now fails the rebuild; the old complete
snapshot stays live. A field-qualified query absent from the active schema is a
client error (HTTP 400), not a reason to fall back to title matching. Active
search is built in-process with the graph snapshot.

Use direct evidence from [[Service Access]], [[Backend API]], [[Browser Regression]],
[[Kueue Scheduling]], and [[Observability]]. Fix the declarative owner instead
of adding host-local repair scripts.

## Cluster DNS (nixstation, 2026-08-11)

"External DNS doesn't work in pods" was two independent stacked faults:

1. **CoreDNS forwarded to dead corporate resolvers.** The k3s Corefile uses
   `forward . /etc/resolv.conf`, which snapshots the *node's* resolv.conf at
   pod start. The `pdx-nxst-*` nodes list `172.19.6.53 / 172.19.6.50 /
   172.18.6.53`, which answer nothing; `pdx-nxnx-lv02` had been hand-fixed to
   `1.1.1.1 / 8.8.8.8 / 9.9.9.9`, so breakage flapped with CoreDNS pod
   placement. Imperative mitigation (survives until k3s re-renders the addon,
   e.g. on upgrade): patch `kube-system/configmap/coredns` to
   `forward . 1.1.1.1 8.8.8.8 9.9.9.9` and `rollout restart
   deployment/coredns`. The durable fix is declarative nameservers in the
   NixOS node configs.
2. **musl libc aborts search traversal on NODATA.** The node search list ends
   with `schrodinger.com`, whose public zone returns NOERROR-with-zero-answers
   for *every* name. musl (Alpine, curlimages, busybox-musl) treats that as a
   hard failure and never tries the literal name, so musl pods get
   `Could not resolve host` for all external names while glibc pods and
   `nslookup` succeed. Pod-level fix: `dnsConfig.options: [{name: ndots,
   value: "1"}]` so dotted names resolve literally before the search list.
   `dnsConfig.searches` cannot remove inherited suffixes — kubelet only
   appends. graph-api itself is glibc-linked and uses the system resolver via
   reqwest/rustls, so it is unaffected.

Diagnosis recipe: compare `nslookup <name>` (bypasses libc, ignores the search
list) against the app's libc resolver, then query CoreDNS metrics
(`kubectl -n kube-system port-forward <coredns-pod> 9153`) and read the
`to="..."` labels to see the *actual* upstreams — do not trust the node's
current resolv.conf to match what a days-old CoreDNS pod inherited.

## Importer health

The [[GitHub Importer]] poll loop surfaces every stage on `/progress`; a
failed fetch leaves the prior complete revision live, so a stale corpus with
no error events means the poll itself is not running. Importer internals are
in [[Importer Runtime]]; search/index behavior in [[Nodes Search and Documents]].
