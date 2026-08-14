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
  {{- /* github, like kubernetes, is a non-filesystem kind: it reads its
         corpus from a remote tarball into an ephemeral pod cache, so
         usesFilesystem/usesObsidian/allowsSeeds must all stay false and the
         vault PVC plus seed init container are skipped. */ -}}
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
{{- $seenVolumeNames := dict -}}
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
    {{- /* Every filesystem source is mounted read-only in the pod (dormant
           ones back runtime source switching), so each must declare its own
           unique volumeName and existingClaim, not only the selected one. */ -}}
    {{- $volumeName := required (printf "importers.sources[%q].source.volumeName is required" $id) $source.volumeName -}}
    {{- if has $volumeName (list "vault-knowledge" "vault-seed" "kubernetes-importer-config" "kubernetes-api-access") -}}
      {{- fail (printf "importers.sources[%q].source.volumeName %q is reserved by the chart" $id $volumeName) -}}
    {{- end -}}
    {{- if hasKey $seenVolumeNames $volumeName -}}
      {{- fail (printf "importers.sources[%q].source.volumeName %q duplicates importers.sources[%q]: volume names must be unique across the catalog" $id $volumeName (index $seenVolumeNames $volumeName)) -}}
    {{- end -}}
    {{- $_ := set $seenVolumeNames $volumeName $id -}}
    {{- $_ = required (printf "importers.sources[%q].source.existingClaim is required" $id) $source.existingClaim -}}
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
Session-manager helpers. The per-world GPU broker defaults its compute
namespace to the graphCompute compute namespace so per-world RayClusters land
in the same standing LocalQueue envelope as the single-tenant session.
*/}}
{{- define "jump-cannon.sessionManagerName" -}}
{{- printf "%s-session-manager" (include "jump-cannon.fullname" .) -}}
{{- end -}}

{{- define "jump-cannon.sessionManagerImage" -}}
{{- printf "%s:%s" .Values.sessionManager.image.repository .Values.sessionManager.image.tag -}}
{{- end -}}

{{- define "jump-cannon.smComputeNamespace" -}}
{{- default (include "jump-cannon.computeNamespace" .) .Values.sessionManager.gpu.namespace -}}
{{- end -}}

{{- define "jump-cannon.worldsClaimName" -}}
{{- default (printf "%s-worlds" (include "jump-cannon.fullname" .)) .Values.sessionManager.persistence.existingClaim -}}
{{- end -}}

{{/*
Guardrails for the session-manager GPU broker: one in-memory controller set
per release, and never two GPU controllers (legacy single-tenant session and
per-world broker) managing the same compute namespace.
*/}}
{{- define "jump-cannon.validateSessionManagerGpu" -}}
{{- if and .Values.sessionManager.enabled .Values.sessionManager.gpu.enabled -}}
{{- if ne (int .Values.sessionManager.replicas) 1 -}}
{{- fail "sessionManager.gpu.enabled requires sessionManager.replicas == 1: in-memory per-world GPU controllers in two replicas would fight over the same RayClusters" -}}
{{- end -}}
{{- if and .Values.graphCompute.enabled .Values.graphCompute.session.enabled -}}
{{- fail "graphCompute.session.enabled and sessionManager.gpu.enabled are mutually exclusive: two GPU controllers would manage the same compute namespace" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
TerminusDB world-store backend helpers.
*/}}
{{- define "jump-cannon.terminusdbName" -}}
{{- printf "%s-terminusdb" (include "jump-cannon.fullname" .) -}}
{{- end -}}

{{/*
Guardrails for the terminusdb store: the admin password must come from a
user-created Secret (never values), and selecting the terminusdb world store
requires the in-cluster server.
*/}}
{{- define "jump-cannon.validateTerminusdb" -}}
{{- if and .Values.terminusdb.enabled (not .Values.terminusdb.adminPasswordSecret.name) -}}
{{- fail "terminusdb.enabled requires terminusdb.adminPasswordSecret.name: the admin password must come from a user-created Secret, never from values" -}}
{{- end -}}
{{- if and .Values.sessionManager.enabled (eq .Values.sessionManager.store "terminusdb") (not .Values.terminusdb.enabled) -}}
{{- fail "sessionManager.store=terminusdb requires terminusdb.enabled=true: the session-manager needs an in-cluster TerminusDB server" -}}
{{- end -}}
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
{{- include "jump-cannon.rayclusterManifestNamed" (dict "root" . "clusterName" (include "jump-cannon.computeName" .)) -}}
{{- end -}}

{{/*
Parameterized form of jump-cannon.rayclusterManifest: takes a dict with
"root" (the template context), "clusterName" (metadata.name — a placeholder
like __world__ for the session-manager per-world template, which the GPU
broker stamps per world at dispatch time), an optional "namespace" override
(sessionManager.gpu.namespace for per-world clusters), and an optional
"maxExecSeconds" override (sessionManager.gpu.maxExecTimeSeconds).
*/}}
{{- define "jump-cannon.rayclusterManifestNamed" -}}
{{- $root := .root -}}
apiVersion: ray.io/v1
kind: RayCluster
metadata:
  name: {{ .clusterName }}
  namespace: {{ .namespace | default (include "jump-cannon.computeNamespace" $root) }}
  labels:
    {{- include "jump-cannon.labels" $root | nindent 4 }}
    app.kubernetes.io/component: graph-compute
    kueue.x-k8s.io/queue-name: {{ $root.Values.graphCompute.ray.queueName | quote }}
    kueue.x-k8s.io/priority-class: {{ $root.Values.graphCompute.ray.workloadPriorityClassName | quote }}
    kueue.x-k8s.io/max-exec-time-seconds: {{ (.maxExecSeconds | default (default 14400 $root.Values.graphCompute.session.maxExecTimeSeconds)) | quote }}
  annotations:
    ai-gateway.schrodinger.com/ttl-seconds: {{ $root.Values.graphCompute.ray.ttlSeconds | quote }}
spec:
  rayVersion: {{ $root.Values.graphCompute.ray.rayVersion | quote }}
  headGroupSpec:
    rayStartParams:
      dashboard-host: "0.0.0.0"
      num-cpus: "0"
      num-gpus: "0"
    template:
      metadata:
        labels:
          {{- include "jump-cannon.selectorLabels" $root | nindent 10 }}
          app.kubernetes.io/component: graph-compute
      spec:
        serviceAccountName: {{ $root.Values.graphCompute.serviceAccountName | quote }}
        automountServiceAccountToken: false
        priorityClassName: {{ $root.Values.graphCompute.ray.podPriorityClassName | quote }}
        imagePullSecrets:
          {{- toYaml $root.Values.imagePullSecrets | nindent 10 }}
        nodeSelector:
          {{- toYaml $root.Values.graphCompute.ray.nodeSelector | nindent 10 }}
        securityContext:
          {{- toYaml $root.Values.graphCompute.podSecurityContext | nindent 10 }}
        containers:
          - name: ray-head
            image: {{ $root.Values.graphCompute.ray.image | quote }}
            imagePullPolicy: IfNotPresent
            env:
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
              {{- toYaml $root.Values.graphCompute.ray.headResources | nindent 14 }}
          - name: graph-compute
            image: {{ include "jump-cannon.graphComputeImage" $root | quote }}
            imagePullPolicy: {{ $root.Values.graphCompute.image.pullPolicy }}
            ports:
              - name: grpc
                containerPort: {{ $root.Values.graphCompute.service.port }}
                protocol: TCP
            env:
              - name: GRAPH_COMPUTE_TICK_HZ
                value: {{ $root.Values.graphCompute.tickHz | quote }}
              - name: GRAPH_COMPUTE_ADDR
                value: {{ $root.Values.graphCompute.bindAddr | quote }}
              - name: RUST_LOG
                value: {{ $root.Values.graphCompute.rustLog | quote }}
            resources:
              {{- toYaml $root.Values.graphCompute.ray.backendResources | nindent 14 }}
            securityContext:
              {{- toYaml $root.Values.containerSecurityContext | nindent 14 }}
{{- end -}}

{{/*
okf-sync: derive the writable OKF repository claim and mount from the importer
catalog so the CronJob can never drift from the profile it feeds. Uses the
selected profile when it is a named OKF filesystem source; otherwise requires
exactly one OKF profile in the catalog. Fails when no runnable OKF filesystem
profile exists, and fails when the derived claim is the vault claim — the
active vault must never be mounted read-write by a batch job. Catalog
validation (jump-cannon.importerSelection) runs first, so source.existingClaim
and source.mountPath are guaranteed present for OKF profiles.
*/}}
{{- define "jump-cannon.okfSyncSource" -}}
{{- $selection := include "jump-cannon.importerSelection" . | fromJson -}}
{{- $profileId := "" -}}
{{- if and $selection.named (eq $selection.kind "okf") -}}
  {{- $profileId = $selection.selected -}}
{{- else -}}
  {{- $found := list -}}
  {{- range $id, $profile := .Values.importers.sources -}}
    {{- if and (kindIs "map" $profile) (eq (default "" $profile.kind) "okf") -}}
      {{- $found = append $found $id -}}
    {{- end -}}
  {{- end -}}
  {{- if ne (len $found) 1 -}}
    {{- fail (printf "okfSync.enabled requires the selected importer to be a named OKF profile or exactly one OKF profile in importers.sources (found %d)" (len $found)) -}}
  {{- end -}}
  {{- $profileId = index $found 0 -}}
{{- end -}}
{{- $profile := index .Values.importers.sources $profileId -}}
{{- $claimName := $profile.source.existingClaim -}}
{{- if eq $claimName (include "jump-cannon.vaultClaimName" .) -}}
  {{- fail (printf "okfSync derives claim %q from importers.sources[%q], which is the vault claim: never mount the active vault read-write" $claimName $profileId) -}}
{{- end -}}
{{- if eq (trim .Values.okfSync.repoUrl) "" -}}
  {{- fail "okfSync.enabled requires okfSync.repoUrl" -}}
{{- end -}}
{{- if eq (trim .Values.okfSync.deployKeySecret) "" -}}
  {{- fail "okfSync.enabled requires okfSync.deployKeySecret: the read-only deploy key must come from a user-created Secret, never from values" -}}
{{- end -}}
{{- dict
    "profileId" $profileId
    "volumeName" $profile.source.volumeName
    "claimName" $claimName
    "mountPath" $profile.source.mountPath
  | toJson -}}
{{- end -}}

{{- define "jump-cannon.okfSyncImage" -}}
{{- printf "%s:%s" .Values.okfSync.image.repository .Values.okfSync.image.tag -}}
{{- end -}}

{{- define "jump-cannon.testImage" -}}
{{- printf "%s:%s" .Values.tests.image.repository .Values.tests.image.tag -}}
{{- end -}}
