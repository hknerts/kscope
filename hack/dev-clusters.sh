#!/usr/bin/env bash
# Local clusters to develop kscope against: two "staging" and two "prod" kind
# clusters with metrics-server and a few workloads that log, so logs, metrics,
# events, triage and fleets all have something real to show.
#
#   hack/dev-clusters.sh up      # create (idempotent)
#   hack/dev-clusters.sh down    # delete
#
# Then: kscope -u stg, kscope get pods -u prod, ...
# Needs kind and kubectl, and a container runtime kind can use.
set -euo pipefail

CLUSTERS=(stg-eu stg-us prod-eu prod-us)
METRICS_SERVER=https://github.com/kubernetes-sigs/metrics-server/releases/latest/download/components.yaml

up() {
  for name in "${CLUSTERS[@]}"; do
    if kind get clusters 2>/dev/null | grep -qx "$name"; then
      echo "· $name exists"
    else
      kind create cluster --name "$name" --wait 120s
    fi
  done
  for name in "${CLUSTERS[@]}"; do seed "kind-$name" "${name%%-*}" "${name#*-}" & done
  wait
  echo
  echo "Ready. Try:  kscope -u stg     kscope get deploy -u prod"
}

seed() {
  local ctx=$1 env=$2 region=$3
  kubectl --context "$ctx" apply -f "$METRICS_SERVER" >/dev/null
  # kind's kubelets serve self-signed certificates. Patch once, so running
  # `up` again does not keep appending the flag.
  if ! kubectl --context "$ctx" -n kube-system get deployment metrics-server \
      -o jsonpath='{.spec.template.spec.containers[0].args}' | grep -q kubelet-insecure-tls; then
    kubectl --context "$ctx" -n kube-system patch deployment metrics-server --type=json \
      -p '[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]' >/dev/null
  fi
  kubectl --context "$ctx" create namespace shop --dry-run=client -o yaml | kubectl --context "$ctx" apply -f - >/dev/null
  for app in checkout cart; do
    kubectl --context "$ctx" -n shop apply -f - >/dev/null <<YAML
apiVersion: apps/v1
kind: Deployment
metadata: {name: $app}
spec:
  replicas: 2
  selector: {matchLabels: {app: $app}}
  template:
    metadata: {labels: {app: $app, env: $env}}
    spec:
      containers:
        - name: app
          image: busybox:1.36
          command: ["sh", "-c", "i=0; while true; do i=\$((i+1)); echo \"\$(date -u +%FT%TZ) INFO $app $env-$region request \$i ok\"; [ \$((i % 7)) -eq 0 ] && echo \"\$(date -u +%FT%TZ) ERROR $app $env-$region upstream timeout\"; sleep 2; done"]
YAML
  done
  # Something in trouble, for `!` to find: one crash-looping pod per staging cluster.
  if [ "$env" = stg ]; then
    kubectl --context "$ctx" -n shop delete pod payments-broken --ignore-not-found >/dev/null
    kubectl --context "$ctx" -n shop run payments-broken --image=busybox:1.36 --restart=Always \
      -- sh -c 'echo "FATAL config missing: PAYMENTS_URL"; exit 1' >/dev/null
  fi
  echo "· $ctx seeded"
}

down() {
  for name in "${CLUSTERS[@]}"; do kind delete cluster --name "$name"; done
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  *) echo "usage: $0 up|down" >&2; exit 2 ;;
esac
