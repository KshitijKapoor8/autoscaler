BIN     := cargo run --
BIN_REL := ./target/release/autoscaler

# ── build ───────────────────────────────────────────────────────────────────────────────

.PHONY: build
build:
	cargo build

.PHONY: build-release
build-release:
	cargo build --release

# ── controller + proxy ──────────────────────────────────────────────────────────
#
# Two modes:
#
#   make http-control-cgroup*   (RECOMMENDED on Linux)
#     Wraps in `systemd-run --user --scope` to get a delegated cgroup.
#     Vertical scaling = OS-level CPU throttle (cpu.max) -- no worker restarts.
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

SCOPE  := systemd-run --user --scope --unit=autoscaler-ctl
# Raise fd limit — the proxy+loadgen open 2 fds per in-flight connection.
# At 100 RPS × 500ms latency = ~50 concurrent connections = ~100 fds minimum.
ULIMIT := ulimit -n 65536 &&


.PHONY: http-control-cgroup
http-control-cgroup:
	$(ULIMIT) $(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cgroup-cpu
http-control-cgroup-cpu:
	$(ULIMIT) $(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 1.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cgroup-mem
http-control-cgroup-mem:
	$(ULIMIT) $(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 1.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cgroup-mixed
http-control-cgroup-mixed:
	$(ULIMIT) $(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control
http-control:
	$(ULIMIT) $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-cpu
http-control-cpu:
	$(ULIMIT) $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 1.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-mem
http-control-mem:
	$(ULIMIT) $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 1.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: http-control-mixed
http-control-mixed:
	$(ULIMIT) $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

# ── CSV-logging variants ──────────────────────────────────────────────────────
# Adds --csv so time-series data is written to data/ alongside console output.
# Two files per run:
#   data/http_ctl_<ts>_summary.csv   one row/second: rps, workers, latency, scale events
#   data/http_ctl_<ts>_workers.csv   one row/second/worker: port, cpu_quota, active, queued

.PHONY: http-control-cpu-csv
http-control-cpu-csv:
	$(ULIMIT) $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 1.0 \
		--initial-replicas 1 --duration 180 --control-interval 10 --csv

.PHONY: http-control-cgroup-csv
http-control-cgroup-csv:
	$(ULIMIT) $(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10 --csv

.PHONY: http-control-cgroup-cpu-csv
http-control-cgroup-cpu-csv:
	$(ULIMIT) $(SCOPE) -- $(BIN) http-control --proxy-port 8080 --base-port 9000 \
		--cpu-factor 2.0 --mem-factor 1.0 \
		--initial-replicas 1 --duration 180 --control-interval 10 --csv


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

# ── load generator ──────────────────────────────────────────────────────────────
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
		--pattern burst --base-rps 5 --peak-rps 100 \
		--burst-start 20 --burst-end 70 --duration 180

.PHONY: loadgen-mem
loadgen-mem:
	$(BIN) loadgen --base-url $(URL) --endpoint mem-heavy \
		--pattern steady --base-rps 20 --duration 120

.PHONY: loadgen-mixed
loadgen-mixed:
	$(BIN) loadgen --base-url $(URL) --endpoint mixed \
		--pattern steady --base-rps 20 --duration 120

# ── aggressive load patterns ───────────────────────────────────────────────────────────
# These use much higher RPS to actually saturate a single worker and force
# the autoscaler to both scale out AND scale up.

# Ramp: 5 RPS linearly climbing to 120 RPS over 60s, then holds.
# Tests: does the controller keep up with gradual saturation?
.PHONY: loadgen-ramp
loadgen-ramp:
	$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
		--pattern ramp --base-rps 5 --peak-rps 120 --ramp-duration 60 --duration 180

# Sawtooth: 5→120 RPS in 40s cycles, instant reset. Repeating pressure waves.
# Tests: oscillation avoidance, recovery speed between waves.
.PHONY: loadgen-sawtooth
loadgen-sawtooth:
	$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
		--pattern sawtooth --base-rps 5 --peak-rps 120 --period 40 --duration 200

# Wave: sinusoidal 40 ± 60 RPS (so 0–100 RPS), 50s period.
# Tests: does the controller over/undershoot on smooth oscillating load?
.PHONY: loadgen-wave
loadgen-wave:
	$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
		--pattern wave --wave-center 40 --peak-rps 60 --period 50 --duration 200

# Double-burst: two separate spikes (t=20-60, t=100-140) at 100 RPS.
# Tests: recovery between events, scale-down speed.
.PHONY: loadgen-double-burst
loadgen-double-burst:
	$(BIN) loadgen --base-url $(URL) --endpoint cpu-heavy \
		--pattern double-burst --base-rps 5 --peak-rps 100 \
		--burst-start 20 --burst-end 60 \
		--burst2-start 100 --burst2-end 140 --duration 180

# ── Docker-backed controller ─────────────────────────────────────────────────
#
# Workers run as Docker containers on the autoscaler-net bridge network.
# Proxy still runs on the host at :8080.
# Vertical scaling = `docker update --cpus` (in-place, no restart).
# Horizontal scaling = `docker run` / `docker rm`.
#
# Prerequisites:
#   1.  Docker is installed and your user can run `docker` without sudo
#   2.  Run `make docker-build` once before starting containers

.PHONY: docker-build
docker-build:
	docker build -t autoscaler:dev .

.PHONY: docker-control
docker-control:
	$(ULIMIT) $(BIN) docker-control --proxy-port 8080 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: docker-control-cpu
docker-control-cpu:
	$(ULIMIT) $(BIN) docker-control --proxy-port 8080 \
		--cpu-factor 2.0 --mem-factor 1.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: docker-control-mem
docker-control-mem:
	$(ULIMIT) $(BIN) docker-control --proxy-port 8080 \
		--cpu-factor 1.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: docker-control-mixed
docker-control-mixed:
	$(ULIMIT) $(BIN) docker-control --proxy-port 8080 \
		--cpu-factor 2.0 --mem-factor 2.0 \
		--initial-replicas 1 --duration 180 --control-interval 10

.PHONY: docker-control-cpu-csv
docker-control-cpu-csv:
	$(ULIMIT) $(BIN) docker-control --proxy-port 8080 \
		--cpu-factor 2.0 --mem-factor 1.0 \
		--initial-replicas 1 --duration 180 --control-interval 10 --csv

# Remove all autoscaler worker containers and the bridge network.
.PHONY: docker-clean
docker-clean:
	docker ps -a --filter "name=autoscaler-worker" -q | xargs -r docker rm -f
	docker network rm autoscaler-net 2>/dev/null || true



.PHONY: sim-steady sim-step sim-burst sim-growth sim-workday sim-flash-sale sim-all
sim-steady:    ; $(BIN) scenario steady
sim-step:      ; $(BIN) scenario step
sim-burst:     ; $(BIN) scenario burst
sim-growth:    ; $(BIN) scenario growth
sim-workday:   ; $(BIN) scenario workday
sim-flash-sale: ; $(BIN) scenario flash-sale
sim-all:       ; $(BIN) all

# ── quick checks ───────────────────────────────────────────────────────────────────

.PHONY: healthz metrics
healthz: ; curl -s http://127.0.0.1:8080/healthz
metrics:  ; curl -s http://127.0.0.1:8080/metrics

# ── help ─────────────────────────────────────────────────────────────────────────────────

.PHONY: help
help:
	@echo ""
	@echo "Build"
	@echo "  make build / build-release"
	@echo ""
	@echo "Controller + proxy (cgroup mode -- RECOMMENDED, Linux/systemd)"
	@echo "  make http-control-cgroup        baseline"
	@echo "  make http-control-cgroup-cpu    cpu_factor=2  (CPU-heavy workload)"
	@echo "  make http-control-cgroup-mem    mem_factor=2  (memory-heavy workload)"
	@echo "  make http-control-cgroup-mixed  cpu+mem heavy"
	@echo ""
	@echo "  Vertical scaling = OS cpu.max quota updated in-place (no worker restart)."
	@echo "  Exactly how Kubernetes CPU limits work."
	@echo ""
	@echo "Controller + proxy (fallback -- no systemd-run required)"
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
	@echo "Docker-backed controller  (requires: make docker-build first)"
	@echo "  make docker-build            build autoscaler:dev image"
	@echo "  make docker-control          baseline workload"
	@echo "  make docker-control-cpu      cpu_factor=2.0"
	@echo "  make docker-control-mem      mem_factor=2.0"
	@echo "  make docker-control-mixed    cpu+mem heavy"
	@echo "  make docker-control-cpu-csv  csv logging"
	@echo "  make docker-clean            remove all worker containers + network"
	@echo ""
	@echo "Simulations"
	@echo "  make sim-steady|sim-step|sim-burst|sim-growth|sim-workday|sim-flash-sale"
	@echo "  make sim-all"
	@echo ""
	@echo "Checks  (against :8080)"
	@echo "  make healthz / metrics"
	@echo ""
	@echo "CSV logging (writes to data/)"
	@echo "  make http-control-cpu-csv        fallback mode + CSV"
	@echo "  make http-control-cgroup-cpu-csv cgroup mode + CSV"
	@echo "  Files: data/http_ctl_<ts>_summary.csv  data/http_ctl_<ts>_workers.csv"
	@echo ""
