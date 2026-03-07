{{/*
Expand the name of the chart.
*/}}
{{- define "reaper.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "reaper.labels" -}}
app.kubernetes.io/part-of: reaper
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version }}
{{- end }}

{{/*
Node component labels
*/}}
{{- define "reaper.node.labels" -}}
{{ include "reaper.labels" . }}
app.kubernetes.io/name: reaper-node
app.kubernetes.io/component: node
{{- end }}

{{/*
Controller component labels
*/}}
{{- define "reaper.controller.labels" -}}
{{ include "reaper.labels" . }}
app.kubernetes.io/name: reaper-controller
app.kubernetes.io/component: controller
{{- end }}
