{{- define "jump-cannon.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "jump-cannon.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "jump-cannon.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "jump-cannon.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "jump-cannon.vaultClaimName" -}}
{{- default (printf "%s-vault" (include "jump-cannon.fullname" .)) .Values.vault.persistence.existingClaim -}}
{{- end -}}

{{/*
Resolve the effective importer once and return it as JSON so every template
uses the same precedence and validation rules. A blank importers.selected is
the compatibility path: kubernetesImporter.enabled still overrides
graphApi.source, and the legacy vault/OKF values remain authoritative.
*/}}
{{- define "jump-cannon.importerSelection" -}}
{{- include "jump-cannon.validateImporterCatalog" . -}}
{{- $selected := default "" .Values.importers.selected -}}
{{- if eq $selected "" -}}
  {{- $kind := ternary "kubernetes" .Values.graphApi.source .Values.kubernetesImporter.enabled -}}
  {{- $rescan := .Values.graphApi.filesystemRescanIntervalSeconds -}}
  {{- if eq $kind "okf" -}}
    {{- $rescan = .Values.okfImporter.filesystemRescanIntervalSeconds -}}
  {{- end -}}
  {{- dict
      "named" false
      "selected" ""
      "kind" $kind
      "sourceId" .Values.okfImporter.sourceId
      "filesystemRescanIntervalSeconds" $rescan
      "usesFilesystem" (or (eq $kind "obsidian") (eq $kind "okf"))
      "usesObsidian" (eq $kind "obsidian")
      "usesKubernetes" .Values.kubernetesImporter.enabled
      "allowsSeeds" (eq $kind "obsidian")
      "volumeName" "vault"
      "claimName" (include "jump-cannon.vaultClaimName" .)
      "mountPath" .Values.vault.mountPath
      "path" .Values.vault.mountPath
      "readOnly" (eq $kind "okf")
      "externalClaim" false
    | toJson -}}
{{- else -}}
  {{- if not (hasKey .Values.importers.sources $selected) -}}
    {{- fail (printf "importers.selected %q does not exist in importers.sources" $selected) -}}
  {{- end -}}
  {{- $profile := index .Values.importers.sources $selected -}}
  {{- if not (kindIs "map" $profile) -}}
    {{- fail (printf "importers.sources[%q] must be an object" $selected) -}}
  {{- end -}}
  {{- $kind := required (printf "importers.sources[%q].kind is required" $selected) $profile.kind -}}
  {{- if not (has $kind (list "obsidian" "kubernetes" "okf")) -}}
    {{- fail (printf "importers.sources[%q].kind %q is not wired by this chart" $selected $kind) -}}
  {{- end -}}
  {{- $usesFilesystem := or (eq $kind "obsidian") (eq $kind "okf") -}}
  {{- $sourceId := default "" $profile.sourceId -}}
  {{- if and (eq $kind "okf") (eq $sourceId "") -}}
    {{- fail (printf "importers.sources[%q].sourceId is required for kind okf" $selected) -}}
  {{- end -}}
  {{- $rescan := default 0 $profile.filesystemRescanIntervalSeconds -}}
  {{- if lt (int $rescan) 0 -}}
    {{- fail (printf "importers.sources[%q].filesystemRescanIntervalSeconds must be non-negative" $selected) -}}
  {{- end -}}
  {{- $volumeName := "" -}}
  {{- $claimName := "" -}}
  {{- $mountPath := "" -}}
  {{- $path := "" -}}
  {{- $readOnly := false -}}
  {{- if $usesFilesystem -}}
    {{- if not (hasKey $profile "source") -}}
      {{- fail (printf "importers.sources[%q].source is required for filesystem kind %s" $selected $kind) -}}
    {{- end -}}
    {{- $source := $profile.source -}}
    {{- if has $source.volumeName (list "vault-knowledge" "vault-seed" "kubernetes-importer-config" "kubernetes-api-access") -}}
      {{- fail (printf "importers.sources[%q].source.volumeName %q is reserved by the chart" $selected $source.volumeName) -}}
    {{- end -}}
    {{- if not (kindIs "map" $source) -}}
      {{- fail (printf "importers.sources[%q].source must be an object" $selected) -}}
    {{- end -}}
    {{- $volumeName = required (printf "importers.sources[%q].source.volumeName is required" $selected) $source.volumeName -}}
    {{- $claimName = required (printf "importers.sources[%q].source.existingClaim is required" $selected) $source.existingClaim -}}
    {{- $mountPath = required (printf "importers.sources[%q].source.mountPath is required" $selected) $source.mountPath -}}
    {{- $path = required (printf "importers.sources[%q].source.path is required" $selected) $source.path -}}
    {{- if has (trimSuffix "/" $mountPath) (list "/knowledge" "/seed") -}}
      {{- fail (printf "importers.sources[%q].source.mountPath %q is reserved by the chart" $selected $mountPath) -}}
    {{- end -}}
    {{- if or (not (hasPrefix "/" $mountPath)) (not (hasPrefix "/" $path)) -}}
      {{- fail (printf "importers.sources[%q].source mountPath and path must be absolute" $selected) -}}
    {{- end -}}
    {{- if or (has "." (splitList "/" $mountPath)) (has ".." (splitList "/" $mountPath)) (has "." (splitList "/" $path)) (has ".." (splitList "/" $path)) -}}
      {{- fail (printf "importers.sources[%q].source mountPath and path must not contain . or .. segments" $selected) -}}
    {{- end -}}
    {{- $childPrefix := printf "%s/" (trimSuffix "/" $mountPath) -}}
    {{- if and (ne $path $mountPath) (not (hasPrefix $childPrefix $path)) -}}
      {{- fail (printf "importers.sources[%q].source.path must equal or be below source.mountPath" $selected) -}}
    {{- end -}}
    {{- if not (hasKey $source "readOnly") -}}
      {{- fail (printf "importers.sources[%q].source.readOnly is required" $selected) -}}
    {{- end -}}
    {{- $readOnly = $source.readOnly -}}
    {{- if and (eq $kind "okf") (not $readOnly) -}}
      {{- fail (printf "importers.sources[%q].source.readOnly must be true for kind okf" $selected) -}}
    {{- end -}}
  {{- end -}}
  {{- dict
      "named" true
      "selected" $selected
      "kind" $kind
      "sourceId" $sourceId
      "filesystemRescanIntervalSeconds" (int $rescan)
      "usesFilesystem" $usesFilesystem
      "usesObsidian" (eq $kind "obsidian")
      "usesKubernetes" (eq $kind "kubernetes")
      "allowsSeeds" (and (eq $kind "obsidian") (not $readOnly))
      "volumeName" $volumeName
      "claimName" $claimName
      "mountPath" $mountPath
      "path" $path
      "readOnly" $readOnly
      "externalClaim" $usesFilesystem
    | toJson -}}
{{- end -}}
{{- end -}}

{{/*
Validate the complete catalog, including dormant profiles. graph-api parses
every item before serving the read-only catalog, so a Helm-valid inactive item
must never turn into a pod startup failure.
*/}}
{{- define "jump-cannon.validateImporterCatalog" -}}
{{- $catalogJson := dict "selected" .Values.importers.selected "sources" .Values.importers.sources | toJson -}}
{{- if gt (len $catalogJson) 65536 -}}
  {{- fail "importers catalog JSON exceeds graph-api's 65536-byte limit" -}}
{{- end -}}
{{- range $id, $profile := .Values.importers.sources -}}
  {{- $kind := required (printf "importers.sources[%q].kind is required" $id) $profile.kind -}}
  {{- if or (eq (trim $profile.displayName) "") (gt (len $profile.displayName) 256) -}}
    {{- fail (printf "importers.sources[%q].displayName must be non-blank and at most 256 UTF-8 bytes" $id) -}}
  {{- end -}}
  {{- if gt (len $profile.description) 4096 -}}
    {{- fail (printf "importers.sources[%q].description exceeds 4096 UTF-8 bytes" $id) -}}
  {{- end -}}
  {{- if not (has $kind (list "obsidian" "kubernetes" "okf")) -}}
    {{- fail (printf "importers.sources[%q].kind %q is not wired by this chart" $id $kind) -}}
  {{- end -}}
  {{- $usesFilesystem := or (eq $kind "obsidian") (eq $kind "okf") -}}
  {{- if and (eq $kind "obsidian") (hasKey $profile "sourceId") -}}
    {{- fail (printf "importers.sources[%q].sourceId is not consumed by the Obsidian importer" $id) -}}
  {{- end -}}
  {{- if eq $kind "kubernetes" -}}
    {{- $sourceId := required (printf "importers.sources[%q].sourceId is required for kind kubernetes" $id) $profile.sourceId -}}
    {{- if ne $sourceId $.Values.kubernetesImporter.config.source_id -}}
      {{- fail (printf "importers.sources[%q].sourceId must equal kubernetesImporter.config.source_id" $id) -}}
    {{- end -}}
  {{- end -}}
  {{- if and (not $usesFilesystem) (hasKey $profile "source") -}}
    {{- fail (printf "importers.sources[%q].source is only valid for filesystem importers" $id) -}}
  {{- end -}}
  {{- if $usesFilesystem -}}
    {{- if not (hasKey $profile "source") -}}
      {{- fail (printf "importers.sources[%q].source is required for filesystem kind %s" $id $kind) -}}
    {{- end -}}
    {{- $source := $profile.source -}}
    {{- $mountPath := required (printf "importers.sources[%q].source.mountPath is required" $id) $source.mountPath -}}
    {{- $path := required (printf "importers.sources[%q].source.path is required" $id) $source.path -}}
    {{- if or (gt (len $mountPath) 4096) (gt (len $path) 4096) -}}
      {{- fail (printf "importers.sources[%q].source mountPath and path must not exceed 4096 UTF-8 bytes" $id) -}}
    {{- end -}}
    {{- if or (not (hasPrefix "/" $mountPath)) (not (hasPrefix "/" $path)) -}}
      {{- fail (printf "importers.sources[%q].source mountPath and path must be absolute" $id) -}}
    {{- end -}}
    {{- if or (has "." (splitList "/" $mountPath)) (has ".." (splitList "/" $mountPath)) (has "." (splitList "/" $path)) (has ".." (splitList "/" $path)) -}}
      {{- fail (printf "importers.sources[%q].source mountPath and path must not contain . or .. segments" $id) -}}
    {{- end -}}
    {{- $childPrefix := printf "%s/" (trimSuffix "/" $mountPath) -}}
    {{- if and (ne $path $mountPath) (not (hasPrefix $childPrefix $path)) -}}
      {{- fail (printf "importers.sources[%q].source.path must equal or be below source.mountPath" $id) -}}
    {{- end -}}
    {{- if and (eq $kind "okf") (not $source.readOnly) -}}
      {{- fail (printf "importers.sources[%q].source.readOnly must be true for kind okf" $id) -}}
    {{- end -}}
    {{- if and (eq $kind "obsidian") $source.readOnly -}}
      {{- fail (printf "importers.sources[%q].source.readOnly must be false until Obsidian write grants are source-scoped" $id) -}}
    {{- end -}}
  {{- end -}}
  {{- with $profile.producer -}}
    {{- if not (hasKey $profile "source") -}}
      {{- fail (printf "importers.sources[%q].producer requires a filesystem source" $id) -}}
    {{- end -}}
    {{- if ne .existingClaimValue $profile.source.existingClaim -}}
      {{- fail (printf "importers.sources[%q].producer.existingClaimValue must equal source.existingClaim" $id) -}}
    {{- end -}}
    {{- $root := required (printf "importers.sources[%q].producer.repositoryRoot is required" $id) .repositoryRoot -}}
    {{- $input := required (printf "importers.sources[%q].producer.workflowInput is required" $id) .workflowInput -}}
    {{- if or (eq (trim .chart) "") (gt (len .chart) 256) -}}
      {{- fail (printf "importers.sources[%q].producer.chart must be non-blank and at most 256 UTF-8 bytes" $id) -}}
    {{- end -}}
    {{- if or (eq (trim .existingClaimValuePath) "") (gt (len .existingClaimValuePath) 256) -}}
      {{- fail (printf "importers.sources[%q].producer.existingClaimValuePath must be non-blank and at most 256 UTF-8 bytes" $id) -}}
    {{- end -}}
    {{- if or (gt (len $root) 4096) (gt (len $input) 4096) -}}
      {{- fail (printf "importers.sources[%q] producer paths must not exceed 4096 UTF-8 bytes" $id) -}}
    {{- end -}}
    {{- if or (not (hasPrefix "/" $root)) (not (hasPrefix "/" $input)) -}}
      {{- fail (printf "importers.sources[%q] producer repositoryRoot and workflowInput must be absolute" $id) -}}
    {{- end -}}
    {{- if or (has "." (splitList "/" $root)) (has ".." (splitList "/" $root)) (has "." (splitList "/" $input)) (has ".." (splitList "/" $input)) -}}
      {{- fail (printf "importers.sources[%q] producer paths must not contain . or .. segments" $id) -}}
    {{- end -}}
    {{- $inputPrefix := printf "%s/" (trimSuffix "/" $root) -}}
    {{- if and (ne $input $root) (not (hasPrefix $inputPrefix $input)) -}}
      {{- fail (printf "importers.sources[%q].producer.workflowInput must equal or be below producer.repositoryRoot" $id) -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- end -}}

{{- define "jump-cannon.labels" -}}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: {{ include "jump-cannon.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: jump-cannon
{{- end -}}

{{- define "jump-cannon.selectorLabels" -}}
app.kubernetes.io/name: {{ include "jump-cannon.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "jump-cannon.graphApiImage" -}}
{{- printf "%s:%s" .Values.graphApi.image.repository .Values.graphApi.image.tag -}}
{{- end -}}

{{- define "jump-cannon.graphComputeImage" -}}
{{- printf "%s:%s" .Values.graphCompute.image.repository .Values.graphCompute.image.tag -}}
{{- end -}}

{{- define "jump-cannon.computeName" -}}
{{- printf "%s-compute" (include "jump-cannon.fullname" .) -}}
{{- end -}}

{{- define "jump-cannon.computeNamespace" -}}
{{- default .Release.Namespace .Values.graphCompute.namespace -}}
{{- end -}}

{{/*
Single source of truth for the graph-compute RayCluster manifest. Rendered
directly as a CR by raycluster-compute.yaml when
graphCompute.session.enabled=false, and embedded into the gpu-session-template
ConfigMap when session mode is on, so the two paths can never drift.
The kueue.x-k8s.io/max-exec-time-seconds label is the Kueue-enforced hard cap
and must be present in BOTH modes.
*/}}
{{- define "jump-cannon.rayclusterManifest" -}}
apiVersion: ray.io/v1
kind: RayCluster
metadata:
  name: {{ include "jump-cannon.computeName" . }}
  namespace: {{ include "jump-cannon.computeNamespace" . }}
  labels:
    {{- include "jump-cannon.labels" . | nindent 4 }}
    app.kubernetes.io/component: graph-compute
    kueue.x-k8s.io/queue-name: {{ .Values.graphCompute.ray.queueName | quote }}
    kueue.x-k8s.io/priority-class: {{ .Values.graphCompute.ray.workloadPriorityClassName | quote }}
    kueue.x-k8s.io/max-exec-time-seconds: {{ default 14400 .Values.graphCompute.session.maxExecTimeSeconds | quote }}
  annotations:
    ai-gateway.schrodinger.com/ttl-seconds: {{ .Values.graphCompute.ray.ttlSeconds | quote }}
spec:
  rayVersion: {{ .Values.graphCompute.ray.rayVersion | quote }}
  headGroupSpec:
    rayStartParams:
      dashboard-host: "0.0.0.0"
      num-cpus: "0"
      num-gpus: "0"
    template:
      metadata:
        labels:
          {{- include "jump-cannon.selectorLabels" . | nindent 10 }}
          app.kubernetes.io/component: graph-compute
      spec:
        serviceAccountName: {{ .Values.graphCompute.serviceAccountName | quote }}
        automountServiceAccountToken: false
        priorityClassName: {{ .Values.graphCompute.ray.podPriorityClassName | quote }}
        imagePullSecrets:
          {{- toYaml .Values.imagePullSecrets | nindent 10 }}
        nodeSelector:
          {{- toYaml .Values.graphCompute.ray.nodeSelector | nindent 10 }}
        securityContext:
          {{- toYaml .Values.graphCompute.podSecurityContext | nindent 10 }}
        initContainers:
          - name: install-ray-runtime
            image: {{ .Values.graphCompute.ray.image | quote }}
            imagePullPolicy: IfNotPresent
            command:
              - /bin/sh
              - -c
            args:
              - |
                set -eu
                marker=/opt/ray-runtime/.complete-ray{{ .Values.graphCompute.ray.rayVersion }}
                [ -f "$marker" ] && exit 0
                python3 -m pip install --disable-pip-version-check --no-cache-dir --no-deps \
                  --target /opt/ray-runtime \
                  ray=={{ .Values.graphCompute.ray.rayVersion }} msgpack==1.2.1 colorful==0.5.8 aiohttp-cors==0.8.1 \
                  opencensus==0.11.4 opencensus-context==0.1.3 py-spy==0.4.2 \
                  virtualenv==21.6.1 smart-open==8.0.0
                PYTHONPATH=/opt/ray-runtime python3 -c 'import ray; assert ray.__version__ == "{{ .Values.graphCompute.ray.rayVersion }}"'
                touch "$marker"
            volumeMounts:
              - name: ray-runtime
                mountPath: /opt/ray-runtime
        containers:
          - name: ray-head
            image: {{ .Values.graphCompute.ray.image | quote }}
            imagePullPolicy: IfNotPresent
            env:
              - name: PYTHONPATH
                value: /opt/ray-runtime
              - name: PATH
                value: /opt/ray-runtime/bin:/usr/local/bin:/usr/bin:/bin
              - name: RAY_USAGE_STATS_ENABLED
                value: "0"
            ports:
              - name: gcs
                containerPort: 6379
              - name: dashboard
                containerPort: 8265
              - name: client
                containerPort: 10001
              - name: metrics
                containerPort: 8080
            resources:
              {{- toYaml .Values.graphCompute.ray.headResources | nindent 14 }}
            volumeMounts:
              - name: ray-runtime
                mountPath: /opt/ray-runtime
          - name: graph-compute
            image: {{ include "jump-cannon.graphComputeImage" . | quote }}
            imagePullPolicy: {{ .Values.graphCompute.image.pullPolicy }}
            ports:
              - name: grpc
                containerPort: {{ .Values.graphCompute.service.port }}
                protocol: TCP
            env:
              - name: GRAPH_COMPUTE_TICK_HZ
                value: {{ .Values.graphCompute.tickHz | quote }}
              - name: GRAPH_COMPUTE_ADDR
                value: {{ .Values.graphCompute.bindAddr | quote }}
              - name: RUST_LOG
                value: {{ .Values.graphCompute.rustLog | quote }}
            resources:
              {{- toYaml .Values.graphCompute.ray.backendResources | nindent 14 }}
            securityContext:
              {{- toYaml .Values.containerSecurityContext | nindent 14 }}
        volumes:
          - name: ray-runtime
            emptyDir: {}
{{- end -}}

{{- define "jump-cannon.testImage" -}}
{{- printf "%s:%s" .Values.tests.image.repository .Values.tests.image.tag -}}
{{- end -}}
