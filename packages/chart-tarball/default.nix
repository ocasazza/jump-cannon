# Helm chart tarball artifact for the jump-cannon chart.
#
# Built on Hydra as `jump-cannon:main:x86_64-linux.chart-tarball`; the
# k8s-artifacts-publish runcommand hook copies $out/charts/*.tgz to the
# Hydra-host chart cache at /var/lib/hydra-cache/charts/ and to the locked-down
# GCS fallback bucket. Consumers fetch from the CIDR-gated /hydra-cache/charts
# path, not from anonymous GCS.
#
# The chart version stays 0.1.0 (stable object key), but appVersion is stamped
# with the source revision so every source build produces distinct tarball
# bytes: the Flux Bucket source watching gs://it-ops-nixstation-k8s-artifacts
# sees a new artifact revision and the consumer HelmRelease upgrades — and the
# Deployment's chart.appVersion pod annotation rolls the pods onto the new
# `latest` image even when the chart templates themselves did not change.
{ pkgs, sourceRev ? "unknown" }:
pkgs.runCommand "jump-cannon-chart-tarball"
  {
    nativeBuildInputs = [
      pkgs.gnugrep
      pkgs.kubernetes-helm
    ];
  }
  ''
    mkdir -p "$out/charts"
    cp -r ${../../charts/jump-cannon} ./jump-cannon
    chmod -R u+w ./jump-cannon

    helm lint ./jump-cannon

    helm template legacy ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      > legacy.yaml
    grep -Fq 'kind: PersistentVolumeClaim' legacy.yaml
    grep -Fq 'name: JUMP_CANNON_IMPORTER_CATALOG_JSON' legacy.yaml
    grep -Fq 'name: JUMP_CANNON_SOURCE' legacy.yaml
    grep -Fq 'value: "obsidian"' legacy.yaml

    # Dashboards are release-scoped, not importer-scoped: disable them here so
    # the no-ConfigMap guard below keeps covering only importer resources.
    helm template lavender ./jump-cannon \
      -f ./jump-cannon/ci/lavender-ingest-okf-values.yaml \
      --set grafanaDashboards.enabled=false \
      > lavender.yaml
    grep -Fq 'name: JUMP_CANNON_IMPORTER_CATALOG_JSON' lavender.yaml
    grep -Fq 'name: JUMP_CANNON_OKF_SOURCE_ID' lavender.yaml
    grep -Fq 'value: "lavender-ingest"' lavender.yaml
    grep -Fq 'value: "okf"' lavender.yaml
    grep -Fq 'value: "/var/lib/lavender/okf-repository/okf"' lavender.yaml
    grep -Fq 'name: lavender-okf-repository' lavender.yaml
    grep -Fq 'mountPath: /var/lib/lavender/okf-repository' lavender.yaml
    grep -Fq 'claimName: lavender-okf-shared' lavender.yaml
    test "$(grep -Fc 'readOnly: true' lavender.yaml)" -eq 2
    if grep -Eq '^kind: (ConfigMap|PersistentVolumeClaim)$' lavender.yaml; then
      echo "selected lavender-ingest-okf must not render ConfigMaps or PVCs" >&2
      exit 1
    fi
    if grep -Fq 'initContainers:' lavender.yaml; then
      echo "selected lavender-ingest-okf must not seed its read-only source" >&2
      exit 1
    fi

    helm template legacy-kubernetes ./jump-cannon \
      --set kubernetesImporter.enabled=true \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      > legacy-kubernetes.yaml
    grep -Fq 'name: JUMP_CANNON_KUBERNETES_CONFIG' legacy-kubernetes.yaml
    grep -Fq 'value: "kubernetes"' legacy-kubernetes.yaml

    # Grafana dashboards: every dashboards/*.json ships as a labeled ConfigMap
    # for the monitoring stack's sidecar; disabling drops them all. (Match the
    # rendered ConfigMap name/label, not the bare label string — the packaged
    # knowledge notes mention grafana_dashboard in prose.)
    grep -Fq 'grafana_dashboard: "1"' legacy.yaml
    grep -Fq 'jump-cannon-grafana-test-results' legacy.yaml
    helm template no-dashboards ./jump-cannon \
      --set grafanaDashboards.enabled=false \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      > no-dashboards.yaml
    if grep -Fq 'jump-cannon-grafana-test-results' no-dashboards.yaml; then
      echo "grafanaDashboards.enabled=false must not render dashboard ConfigMaps" >&2
      exit 1
    fi

    helm template named-kubernetes ./jump-cannon \
      -f ./jump-cannon/ci/named-kubernetes-values.yaml \
      > named-kubernetes.yaml
    grep -Fq 'name: JUMP_CANNON_KUBERNETES_CONFIG' named-kubernetes.yaml
    grep -Fq 'value: "kubernetes"' named-kubernetes.yaml
    grep -Fq 'in-cluster-kubernetes' named-kubernetes.yaml

    # Static compute mode (default): the RayCluster CR renders with the
    # Kueue max-exec-time safety label.
    helm template gpu-static ./jump-cannon \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      > gpu-static.yaml
    grep -Fq 'kind: RayCluster' gpu-static.yaml
    grep -Fq 'kueue.x-k8s.io/max-exec-time-seconds' gpu-static.yaml

    # On-demand session mode: no RayCluster CR; the template ConfigMap,
    # least-privilege session RBAC, Deployment env/mount, and the always-on
    # compute Service render instead.
    helm template gpu-session ./jump-cannon \
      --set graphCompute.session.enabled=true \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      > gpu-session.yaml
    if grep -Eq '^kind: RayCluster$' gpu-session.yaml; then
      echo "session mode must not render the RayCluster CR" >&2
      exit 1
    fi
    grep -Fq 'gpu-session-template' gpu-session.yaml
    grep -Fq 'rayclusters' gpu-session.yaml
    grep -Fq 'pods/log' gpu-session.yaml
    grep -Fq 'kueue.x-k8s.io/max-exec-time-seconds' gpu-session.yaml
    grep -Fq 'name: JUMP_CANNON_GPU_SESSION_TEMPLATE' gpu-session.yaml
    grep -Fq 'name: JUMP_CANNON_GPU_SESSION_CLUSTER_NAME' gpu-session.yaml
    grep -Fq 'name: JUMP_CANNON_GPU_SESSION_NAMESPACE' gpu-session.yaml
    grep -Fq 'jump-cannon-compute' gpu-session.yaml
    # The session controller needs the in-cluster API; session mode must
    # force service-account token automount on.
    grep -Fq 'automountServiceAccountToken: true' gpu-session.yaml
    if grep -Fq 'rayclusters/finalizers' gpu-session.yaml; then
      echo "session RBAC must not touch RayCluster finalizers" >&2
      exit 1
    fi

    expect_render_failure() {
      name="$1"
      shift
      if helm template "$name" ./jump-cannon "$@" > "$name.error" 2>&1; then
        echo "expected $name render to fail" >&2
        exit 1
      fi
    }

    expect_render_failure unknown-importer \
      --set-string importers.selected=not-in-catalog
    expect_render_failure blank-importer-display-name \
      --set-string 'importers.sources.lavender-ingest-okf.displayName= '
    expect_render_failure reserved-importer-volume-name \
      --set-string importers.sources.lavender-ingest-okf.source.volumeName=vault-knowledge
    expect_render_failure reserved-importer-mount-path \
      --set-string importers.sources.lavender-ingest-okf.source.mountPath=/knowledge \
      --set-string importers.sources.lavender-ingest-okf.source.path=/knowledge/okf
    expect_render_failure invalid-okf-path \
      -f ./jump-cannon/ci/invalid-okf-path-values.yaml
    expect_render_failure invalid-dormant-okf-path \
      -f ./jump-cannon/ci/invalid-dormant-okf-path-values.yaml
    expect_render_failure invalid-okf-read-write \
      -f ./jump-cannon/ci/invalid-okf-read-write-values.yaml
    expect_render_failure invalid-producer-claim \
      -f ./jump-cannon/ci/invalid-producer-claim-values.yaml
    expect_render_failure mismatched-kubernetes-source-id \
      -f ./jump-cannon/ci/named-kubernetes-values.yaml \
      --set-string importers.sources.in-cluster-kubernetes.sourceId=another-cluster
    expect_render_failure unsupported-pest-profile \
      --set-string importers.selected=pest \
      --set-string importers.sources.pest.displayName=Pest \
      --set-string importers.sources.pest.description=unsupported \
      --set-string importers.sources.pest.kind=pest \
      --set importers.sources.pest.filesystemRescanIntervalSeconds=0
    expect_render_failure gpu-session-multi-replica \
      --set graphCompute.session.enabled=true \
      --set graphApi.replicas=2

    helm package --app-version "${sourceRev}" ./jump-cannon -d "$out/charts"
  ''
