# Autoscaler Simulation

A Rust-based closed-loop simulation of a unified autoscaler that controls both horizontal and vertical scaling of a service under load.

### Overview

This simulation models:
- **Workload**: Variable incoming request rate (RPS)
- **System**: Pool of replicas processing requests with queuing
- **Metrics**: Latency, CPU/memory utilization, queue depth
- **Controller**: Unified autoscaler making both horizontal and vertical scaling decisions

### CSV Output Format

When using `--csv`, data is written to `data/<scenario_name>.csv`:

```csv
time_s,rps,queue,replicas,cpu_per_replica,mem_per_replica,cpu_util,mem_util,latency_ms,action
1,100,0,1,1,2,100,50,10,-
2,100,0,1,1,2,100,50,10,-
...
```
