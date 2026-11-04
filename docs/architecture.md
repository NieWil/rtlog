# How rtlog works

Three stages. At **build** time, each call site gets an ID and a descriptor. At **runtime**, each thread writes raw binary frames into its own ring. On the **host**, the decoder joins the stream with the contract.

```mermaid
%%{init: {"theme": "base", "themeVariables": {"fontSize": "14px", "primaryColor": "#eef2ff", "primaryBorderColor": "#6366f1", "primaryTextColor": "#1e1b4b", "lineColor": "#94a3b8", "clusterBkg": "#f8fafc", "clusterBorder": "#cbd5e1"}}}%%
flowchart LR
  subgraph B["Build"]
    direction TB
    M["rtlog::info!(…)"] --> S[("site descriptor<br/>ID · level · fmt")]
  end
  subgraph T["Target · MCU, Linux, desktop"]
    direction TB
    T1["thread / task"] --> R1["SPSC ring"]
    T2["thread / task"] --> R2["SPSC ring"]
    R1 --> D(["drain"])
    R2 --> D
  end
  subgraph H["Host"]
    direction TB
    C[("contract")] --> DEC{{"rtlog decode"}}
    DEC --> O1["text"]
    DEC --> O2["JSON lines"]
    DEC --> O3["Perfetto"]
  end
  S -. "rtlog contract --elf" .-> C
  D == "RTL1 stream: UART · RTT · file · socket" ==> DEC
  classDef hot fill:#fef3c7,stroke:#f59e0b,color:#78350f
  classDef host fill:#dcfce7,stroke:#16a34a,color:#14532d
  class R1,R2,D hot
  class DEC,O1,O2,O3 host
```

## One log call

```mermaid
%%{init: {"theme": "base", "themeVariables": {"actorBkg": "#eef2ff", "actorBorder": "#6366f1", "actorTextColor": "#1e1b4b", "signalColor": "#475569", "noteBkgColor": "#fef3c7", "noteBorderColor": "#f59e0b"}}}%%
sequenceDiagram
  autonumber
  participant App as Your thread
  participant Ring as Its ring
  participant Drain as Drain (idle / thread)
  participant Out as Transport
  App->>App: level check (one relaxed load)
  App->>App: encode args into a stack buffer
  App->>Ring: copy frame, then publish head (release store)
  Note over App,Ring: no lock, no allocation, no syscall, no formatting
  Drain->>Ring: read head (acquire), copy whole frames
  Drain->>Out: write frame
  Drain->>Ring: publish tail (release store)
  Note over Drain,Out: the only place that may block or batch
```

## The frame

| Field | Size | Content |
| --- | --- | --- |
| site ID | 4 B | from the contract: content hash (Rust) or descriptor index (C) |
| timestamp delta | varint, 1–10 B | ticks since this ring's previous frame |
| context | varint, 1–5 B | which thread / task / ISR |
| arguments | per argument: 1 B type tag + value | integers and floats raw little-endian; strings as length + bytes |

A W1 call (`info!("x={} y={}", u32, u32)`) is 18 bytes on the wire; the same line as text is 33. Control frames announce contexts, report drops and open and close spans. The full grammar, design decisions and threat model are in the [specification](spec.md); what v0.2 implements and where it differs is in the [implementation notes](implementation.md).

## Guarantees

- **Single producer, single consumer per ring.** There is no CAS and no spin. Producers never share a cache line with each other.
- **Lossy, never blocking.** A full ring drops the new frame and counts it. The drain reports the count, so the host sees every loss.
- **Untrusted input on the host.** The decoder bounds every length and allocation and stops cleanly at the first bad frame. A corruption sweep in CI checks this.
