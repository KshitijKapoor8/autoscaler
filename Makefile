BIN     := cargo run --
BIN_REL := ./target/release/autoscaler

# ── build ─────────────────────────────────────────────────────────────────────

.PHONY: build
build:
cargo build

.PHONY: build-release
build-release:
cargo build --release

# ── controller + proxy ────────────────────────────────────────────────────────
#
# Two modes:
#
#   make http-control-cgroup*   (RECOMMENDED on Linux)
#     Wraps in `systemd-run --user --scope` to get a delegated cgroup.
#     Vertical scaling = OS-level CPU throttle (cpu.max) — no worker restarts.
#     Exactly how Kubernetes CPU limits work.
#
#   make http-control*          (fallback)
#     No systemd-run.  Vertical scaling = restart workers with different
#     concurrency slot count.  Less realistic but works everywhere.
#
# The binary detects at runtime which mode is available and logs accordingly.
#
# Proxy listens on :8080 (stable).  Workers are on :9000+.
# Point the load generator at :8080.

SCOPE := systemd-run --user --scope --unit=autoscaler-ctl

.PHONY: http-control-cgroup
http-control-cgroup:
$(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cgroup-cpu
http-control-cgroup-cpu:
$(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
--cpu-factor 2.0 --mem-factor 1.0 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cgroup-mem
http-control-cgroup-mem:
$(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
--cpu-factor 1.0 --mem-factor 2.0 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cgroup-mixed
http-control-cgroup-mixed:
$(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
--cpu-factor 2.0 --mem-factor 2.0 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control
http-control:
$(BIN) http-control --proxy-port 8080 --base-port 9000 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cpu
http-control-cpu:
$(BIN) http-control --proxy-port 8080 --base-port 9000 \
--cpu-factor 2.0 --mem-factor 1.0 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-mem
http-control-mem:
$(BIN) http-control --proxy-port 8080 --base-port 9000 \
--cpu-factor 1.0 --mem-factor 2.0 \
--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-mixed
http-control-mixed:
$(BIN) http-control --proxy-port 8080 --base-port 9000 \
--cpu-factor 2.0 --mem-factor 2.0 \
--initial-replicas 1 --duration 180 --control-interval 10

# ── standalone service (single worker, no controller) ─────────────────────────

.PHONY: service
service:
$(BIN) service --port 8080 --max-concurrency 32

.PHONY: service-cpu-heavy
service-cpu-heavy:
$(BIN) service --port 8080 --cpu-factor 2.0 --max-concurrency 32

.PHONY: service-mem-heavy
service-mem-heavy:
$(BIN) service --port 8080 --mem-factor 2.0 --max-concurrency 32

# ── load generator ────────────────────────────────────────────────────────────
# Always targets the proxy on :8080.
# Override: make loadgen-steady URL=http://127.0.0.1:9000

URL ?= http://127.0.0.1:8080

.PHONY: loadgen-steady
loadgen-steady:
$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
--pattern steady --base-rps 20 --duration 120

.PHONY: loadgen-step
loadgen-step:
$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
--pattern step --base-rps 5 --step-to 30 --step-at 30 --duration 120

.PHONY: loadgen-burst
loadgen-burst:
$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
--pattern burst --base-rps 5 --peak-rps 40 \
--burst-start 20 --burst-end 60 --duration 120

.PHONY: loadgen-mem
loadgen-mem:
$(BIN) loadgen --base-url $(URL) --endpoint mem-heavy \
--pattern steady --base-rps 20 --duration 120

.PHONY: loadgen-mixed
loadgen-mixed:
$(BIN) loadgen --base-url $(URL) --endpoint mixed \
--pattern steady --base-rps 20 --duration 120

# ── simulation scenarios ──────────────────────────────────────────────────────

.PHONY: sim-steady sim-step sim-burst sim-growth sim-workday sim-flash-sale sim-all
sim-steady:    ; $(BIN) scenario steady
sim-step:      ; $(BIN) scenario step
sim-burst:     ; $(BIN) scenario burst
sim-growth:    ; $(BIN) scenario growth
sim-workday:   ; $(BIN) scenario workday
sim-flash-sale:; $(BIN) scenario flash-sale
sim-all:       ; $(BIN) all

# ── quick checks ──────────────────────────────────────────────────────────────

.PHONY: healthz metrics
healthz: ; curl -s http://127.0.0.1:8080/healthz
metrics:  ; curl -s http://127.0.0.1:8080/metrics

# ── help ──────────────────────────────────────────────────────────────────────

.PHONY: help
help:
@echo ""
@echo "Build"
@echo "  make build / build-release"
@echo ""
@echo "Controller + proxy (cgroup mode — RECOMMENDED, Linux/systemd)"
@echo "  make http-control-cgroup        baseline"
@echo "  make http-control-cgroup-cpu    cpu_factor=2  (CPU-heavy workload)"
@echo "  make http-control-cgroup-mem    mem_factor=2  (memory-heavy workload)"
@echo "  make http-control-cgroup-mixed  cpu+mem heavy"
@echo ""
@echo "  Vertical scaling = OS cpu.max quota updated in-place (no worker restart)."
@echo "  Exactly how Kubernetes CPU limits work."
@echo ""
@echo "Controller + proxy (fallback — no systemd-run required)"
@echo "  make http-control / http-control-cpu / http-control-mem / http-control-mixed"
@echo ""
@echo "  Vertical scaling = restart workers with new concurrency slot count."
@echo ""
@echo "Standalone service  (no controller, single worker)"
@echo "  make service / service-cpu-heavy / service-mem-heavy"
@echo ""
@echo "Load generator  (targets proxy on :8080)"
@echo "  make loadgen-steady   cpu-heavy, 20 RPS steady, 120s"
@echo "  make loadgen-step     cpu-heavy, 5->30 RPS at t=30s"
@echo "  make loadgen-burst    cpu-heavy, burst 5->40 RPS t=20-60s"
@echo "  make loadgen-mem      mem-heavy, steady"
@echo "  make loadgen-mixed    mixed, steady"
@echo "  Override: make loadgen-steady URL=http://127.0.0.1:9000"
@echo ""
@echo "Simulations"
@echo "  make sim-steady|sim-step|sim-burst|sim-growth|sim-workday|sim-flash-sale"
@echo "  make sim-all"
@echo ""
@echo "Checks  (against :8080)"
@echo "  make healthz / metrics"
@echo ""
