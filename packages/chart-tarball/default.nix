# Helm chart tarball artifact for the jump-cannon chart.
#
# Built on Hydra as `jump-cannon:main:x86_64-linux.chart-tarball`; the
# k8s-artifacts-publish runcommand hook copies $out/charts/*.tgz to the
# Hydra-host chart cache at /var/lib/hydra-cache/charts/ and to the locked-down
# GCS fallback bucket. Consumers fetch from the CIDR-gated /hydra-cache/charts
# path, not from anonymous GCS.
{ pkgs }:
pkgs.runCommand "jump-cannon-chart-tarball"
  {
    nativeBuildInputs = [ pkgs.kubernetes-helm ];
  }
  ''
    mkdir -p "$out/charts"
    cp -r ${../../charts/jump-cannon} ./jump-cannon
    chmod -R u+w ./jump-cannon
    helm package ./jump-cannon -d "$out/charts"
  ''
