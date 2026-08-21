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
      --set tests.k6.enabled=false \
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
    # okfSync defaults to disabled: no sync CronJob may render.
    if grep -Eq '^kind: CronJob$' lavender.yaml; then
      echo "okfSync.enabled=false must not render any CronJob" >&2
      exit 1
    fi

    # okf-sync CronJob: claim and mount are derived from the selected OKF
    # profile, the deploy key comes from the named Secret, and a hard
    # podAffinity pins the sync pod to the graph-api node (RWO local-path).
    # Assertions run against the extracted CronJob document because the claim
    # name and mount path also appear (read-only) in the graph-api Deployment.
    helm template okf-sync ./jump-cannon \
      -f ./jump-cannon/ci/lavender-ingest-okf-values.yaml \
      --set grafanaDashboards.enabled=false \
      --set okfSync.enabled=true \
      > okf-sync.yaml
    awk '/^# Source: jump-cannon\/templates\/okf-sync.yaml$/{f=1} f&&/^---$/{exit} f' \
      okf-sync.yaml > okf-sync-cronjob.yaml
    grep -Fq 'kind: CronJob' okf-sync-cronjob.yaml
    grep -Eq '^  name: okf-sync-jump-cannon-okf-sync$' okf-sync-cronjob.yaml
    grep -Fq 'schedule: "17 * * * *"' okf-sync-cronjob.yaml
    grep -Fq 'concurrencyPolicy: Forbid' okf-sync-cronjob.yaml
    grep -Fq 'image: "us-central1-docker.pkg.dev/it-ops-nixstation/lavender/jump-cannon-okf-sync:latest"' okf-sync-cronjob.yaml
    grep -Fq 'name: OKF_REPO_URL' okf-sync-cronjob.yaml
    grep -Fq 'value: "git@github.com:schrodinger/lavender-okf.git"' okf-sync-cronjob.yaml
    grep -Fq 'name: OKF_TARGET_DIR' okf-sync-cronjob.yaml
    grep -Fq 'value: "/var/lib/lavender/okf-repository"' okf-sync-cronjob.yaml
    grep -Fq 'mountPath: /var/lib/lavender/okf-repository' okf-sync-cronjob.yaml
    grep -Fq 'claimName: lavender-okf-shared' okf-sync-cronjob.yaml
    grep -Fq 'secretName: "lavender-okf-reader"' okf-sync-cronjob.yaml
    grep -Fq 'defaultMode: 0400' okf-sync-cronjob.yaml
    grep -Fq 'requiredDuringSchedulingIgnoredDuringExecution' okf-sync-cronjob.yaml
    grep -Fq 'topologyKey: kubernetes.io/hostname' okf-sync-cronjob.yaml
    grep -Fq 'app.kubernetes.io/component: graph-api' okf-sync-cronjob.yaml
    # The sync pod is the claim's writer: only the deploy-key mount may be
    # read-only inside the CronJob pod.
    test "$(grep -Fc 'readOnly: true' okf-sync-cronjob.yaml)" -eq 1

    # okfSync with a dormant (unselected) OKF profile still derives claim and
    # mount from the catalog's single OKF entry.
    helm template okf-sync-dormant ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      --set okfSync.enabled=true \
      > okf-sync-dormant.yaml
    grep -Fq 'kind: CronJob' okf-sync-dormant.yaml
    grep -Fq 'claimName: lavender-okf-shared' okf-sync-dormant.yaml
    grep -Fq 'value: "/var/lib/lavender/okf-repository"' okf-sync-dormant.yaml

    helm template legacy-kubernetes ./jump-cannon \
      --set kubernetesImporter.enabled=true \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      > legacy-kubernetes.yaml
    grep -Fq 'name: JUMP_CANNON_KUBERNETES_CONFIG' legacy-kubernetes.yaml
    grep -Fq 'value: "kubernetes"' legacy-kubernetes.yaml

    # tests.performance.profiling: PYROSCOPE_URL renders only when enabled,
    # requires a URL when it is, and stays out of the default render.
    helm template profiling ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      --set tests.performance.profiling.enabled=true \
      --set tests.performance.profiling.pyroscopeUrl=http://pyroscope.monitoring.svc:4040 \
      > profiling.yaml
    grep -Fq 'name: PYROSCOPE_URL' profiling.yaml
    grep -Fq 'value: "http://pyroscope.monitoring.svc:4040"' profiling.yaml
    helm template profiling-off ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      > profiling-off.yaml
    if grep -Fq 'name: PYROSCOPE_URL' profiling-off.yaml; then
      echo "PYROSCOPE_URL must not render with tests.performance.profiling.enabled=false" >&2
      exit 1
    fi
    if helm template profiling-missing-url ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      --set tests.performance.profiling.enabled=true \
      > /dev/null 2>&1; then
      echo "tests.performance.profiling.enabled without pyroscopeUrl must fail template" >&2
      exit 1
    fi

    # GitHub docs-importer mode: env from githubImporter values (seconds -> ms
    # for the poll interval), an ephemeral emptyDir extraction cache, and no
    # vault filesystem (github is a non-filesystem source like kubernetes).
    helm template github ./jump-cannon \
      --set graphApi.source=github \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      > github.yaml
    grep -Fq 'value: "github"' github.yaml
    grep -Fq 'name: JUMP_CANNON_GITHUB_REPO' github.yaml
    grep -Fq 'value: "ocasazza/jump-cannon"' github.yaml
    grep -Fq 'name: JUMP_CANNON_GITHUB_REF' github.yaml
    grep -Fq 'value: "main"' github.yaml
    grep -Fq 'name: JUMP_CANNON_GITHUB_PATH' github.yaml
    grep -Fq 'value: "charts/jump-cannon/knowledge"' github.yaml
    grep -Fq 'name: JUMP_CANNON_GITHUB_POLL_INTERVAL_MS' github.yaml
    grep -Fq 'value: "60000"' github.yaml
    grep -Fq 'name: JUMP_CANNON_GITHUB_CACHE_DIR' github.yaml
    grep -Fq 'name: github-importer-cache' github.yaml
    grep -Fq 'mountPath: "/var/cache/jump-cannon/github"' github.yaml
    grep -Fq 'emptyDir: {}' github.yaml
    if grep -Fq 'name: VAULT_ROOT' github.yaml; then
      echo "github mode must not mount the vault filesystem" >&2
      exit 1
    fi
    if grep -Eq '^kind: PersistentVolumeClaim$' github.yaml; then
      echo "github mode must not render the vault PVC" >&2
      exit 1
    fi
    # The token never comes from values: it renders only via secretKeyRef.
    helm template github-token ./jump-cannon \
      --set graphApi.source=github \
      --set githubImporter.tokenSecret.name=jump-cannon-github \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.enabled=false \
      > github-token.yaml
    grep -Fq 'name: JUMP_CANNON_GITHUB_TOKEN' github-token.yaml
    grep -Fq 'secretKeyRef:' github-token.yaml
    grep -Fq 'name: "jump-cannon-github"' github-token.yaml
    if grep -Fq 'name: JUMP_CANNON_GITHUB_TOKEN' github.yaml; then
      echo "github token env must not render without tokenSecret.name" >&2
      exit 1
    fi

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
      --set tests.k6.enabled=false \
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
      --set tests.k6.enabled=false \
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
      --set tests.k6.enabled=false \
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

    # k6 CronJob (Grafana-native HTTP regression): renders only when
    # tests.k6.enabled, mounts the chart-owned script ConfigMap, remote-writes
    # k6_* metrics to the monitoring Prometheus, and is bounded independently
    # of the fuzz/perf shapes. Kueue stays opt-in (a light online check must
    # not queue behind GPU quota).
    helm template k6 ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      > k6.yaml
    grep -Fq 'kind: CronJob' k6.yaml
    grep -Fq 'app.kubernetes.io/component: k6-tests' k6.yaml
    grep -Fq 'image: "grafana/k6:2.2.0"' k6.yaml
    grep -Fq 'experimental-prometheus-rw' k6.yaml
    grep -Fq 'value: "http://prometheus.monitoring.svc.cluster.local:9090/api/v1/write"' k6.yaml
    grep -Fq 'name: k6-script' k6.yaml
    grep -Fq 'jump-cannon-k6-script' k6.yaml
    grep -Fq 'memory: 128Mi' k6.yaml
    grep -Fq 'memory: 256Mi' k6.yaml
    if grep -Fq 'kueue.x-k8s.io/queue-name' k6.yaml; then
      echo "k6 kueue must stay opt-in (no labels at defaults)" >&2
      exit 1
    fi
    helm template k6-kueue ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.fuzz.enabled=false \
      --set tests.performance.enabled=false \
      --set tests.browser.enabled=false \
      --set tests.k6.kueue.enabled=true \
      > k6-kueue.yaml
    grep -Fq 'kueue.x-k8s.io/queue-name: "batch"' k6-kueue.yaml
    grep -Fq 'kueue.x-k8s.io/priority-class: "batch"' k6-kueue.yaml
    grep -Fq 'priorityClassName: "batch"' k6-kueue.yaml

    # Profiling wiring: the fuzz CronJob passes PYROSCOPE_URL + per-run pod
    # id only when tests.fuzz.profiling is enabled; the dashboard ships the
    # cubism + flame-graph panels (Pyroscope datasource).
    helm template profiled ./jump-cannon \
      --set graphCompute.enabled=false \
      --set tests.k6.enabled=false \
      --set-string tests.fuzz.profiling.enabled=true \
      --set-string tests.fuzz.profiling.pyroscopeUrl=http://pyroscope.monitoring.svc.cluster.local:4040 \
      --set-string tests.performance.profiling.enabled=true \
      --set-string tests.performance.profiling.pyroscopeUrl=http://pyroscope.monitoring.svc.cluster.local:4040 \
      > profiled.yaml
    test "$(grep -Fc 'name: PYROSCOPE_URL' profiled.yaml)" -eq 2
    # fuzz profiling carries the explicit per-run id; perf relies on the
    # wrapper's $(hostname) fallback (also the pod name).
    test "$(grep -Fc 'fieldPath: metadata.name' profiled.yaml)" -eq 1
    grep -Fq 'ekacnet-cubismgrafana-panel' legacy.yaml
    grep -Fq '"type": "flamegraph"' legacy.yaml
    grep -Fq 'grafana-pyroscope-datasource' legacy.yaml

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
    # okf-sync must refuse to mount the active vault claim read-write.
    expect_render_failure okf-sync-vault-claim \
      --set okfSync.enabled=true \
      --set-string vault.persistence.existingClaim=lavender-okf-shared
    # okf-sync needs a runnable OKF filesystem profile to derive claim/mount
    # from; with the catalog's only OKF entry turned into Obsidian it fails.
    expect_render_failure okf-sync-no-okf-profile \
      --set okfSync.enabled=true \
      --set importers.sources.lavender-ingest-okf.sourceId=null \
      --set importers.sources.lavender-ingest-okf.producer=null \
      --set-string importers.sources.lavender-ingest-okf.kind=obsidian \
      --set importers.sources.lavender-ingest-okf.source.readOnly=false
    # The deploy key always comes from a user-created Secret, never values.
    expect_render_failure fuzz-profiling-no-url \
      --set graphCompute.enabled=false \
      --set tests.k6.enabled=false \
      --set-string tests.fuzz.profiling.enabled=true
    expect_render_failure okf-sync-blank-deploy-key-secret \
      --set okfSync.enabled=true \
      --set-string okfSync.deployKeySecret=" "

    helm package --app-version "${sourceRev}" ./jump-cannon -d "$out/charts"
  ''
