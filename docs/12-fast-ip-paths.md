# 12 — Fast IP paths: design note

Date: 2026-10-30 · Status: **proposal for a later decision; nothing implemented**

Evidence tags as in `03`. Related:
- `04` (ratflector analysis)
- `05` §5.3 and ADR-0004 (Aether Link Protocol, ALP)
- `02` defects P6, P16, P19, P20

## 0. Summary

Aether DV's internet ports today move data like a slow radio:
- ratflector traffic uses the legacy D-RATS file transfer: 1 KiB blocks, 4 per round trip, one direction at a time;
- every frame goes to every client on the ratflector.

That gives about 40 KB/s at 100 ms round-trip time, about 13 KB/s at 300 ms, whatever the connection could carry [MODEL]. It's fine for chat and ICS-213 forms. It's slow for photos, maps, situation reports with attachments, or a backlog after an outage.

**Proposal: tiered IP paths.** Speed is set by the connection, measured as traffic flows, not by D-RATS's radio-era limits.

| Tier | Path | Peers | Speed | Who else receives it |
|---|---|---|---|---|
| T0 | Ratflector, legacy transfer (today) | D-RATS and Aether DV | ~13-40 KB/s | **Everyone on the ratflector** |
| T1 | Ratflector, tuned legacy transfer | D-RATS and Aether DV | A few times T0 | Everyone on the ratflector |
| T2 | Ratflector **unicast** relay (`aetherdv-rfd`) | Aether DV ↔ Aether DV | Up to the server's per-client limit | Only the addressee |
| T3 | **Direct peer link** (TCP; TLS on the public internet) | Aether DV ↔ Aether DV | The connection's speed | Only the addressee |
| T4 | Amateur IP networks (AREDN mesh, HamWAN) | Aether DV ↔ Aether DV | Mbps, on amateur spectrum | Only the addressee |

Above the tiers, **per-destination path choice with failover**: the fastest path that works now, falling back to RF when the internet is down. For emergency communications that's the point: Aether DV keeps working when the internet doesn't.

## 1. Why internet transfers are slow today

| Limit | Where | Effect |
|---|---|---|
| ~4 KiB in flight per round trip (4 × 1 KiB blocks) | `arq_config("internet")`; D-RATS sender defaults `legacy_defaults()` (512 B × 4) | Throughput ≈ window ÷ RTT |
| 8-bit block numbers; over 256 blocks a D-RATS receiver can lose data unnoticed | `02` P6, P19 | Bigger windows are limited; large files over legacy transfer are risky |
| Half-duplex polled transport in D-RATS | `02` P16 | D-RATS stations can't send and receive at once, even on TCP |
| Ratflector floods every frame to every client | `04` §1 | Bulk transfers load every station on the server; `aetherdv-rfd` limits each client to 64 KiB/s, 256 KiB burst (`server.rs:55`) |

## 2. Design

### 2.1 T1: tuned legacy transfers (D-RATS peers)

Block size and window are chosen by the sender; the receiver accepts what arrives (`02` P6). Toward a D-RATS peer on an internet port, Aether DV could use larger blocks (2-4 KiB) and a window sized from the measured round trip. Limits:
- **The block-number space:** keep transfers well under 256 blocks per cycle, or stay with today's sizes (P19).
- **Unknown D-RATS limits:** whether D-RATS accepts blocks above 1 KiB, and how its frame reader handles them, is **[UNK]**. Test against D-RATS 0.4.x first (FI-1).
- **Courtesy on shared ratflectors:** everyone receives the transfer. A per-ratflector rate cap (default well under the server's 64 KiB/s) and a confirmation for large files ("this sends 2 MB to every station on sewx").

### 2.2 T2: unicast relay through `aetherdv-rfd` (Aether DV ↔ Aether DV)

- Aether DV clients tell the server they can do unicast, using the capability exchange (ADR-0004).
- Frames addressed to one call sign that's connected to the server as an Aether DV client are forwarded **only to that client**, not flooded.
- Broadcast traffic and anything for D-RATS clients still floods, exactly as today.
- **No NAT problems:** both stations already hold outgoing connections to the server.
- The server sets per-client rates, which can be higher for unicast, since it no longer multiplies by the client count.
- Public D-RATS ratflectors don't do this; it applies only on servers running `aetherdv-rfd`.

### 2.3 T3: direct peer links

- An Aether DV station can **listen** for peer links (off by default) on a port the operator chooses.
- A peer reaches it by address, learned from a ratflector rendezvous message (T2) or typed in.
- **TCP carries the reliability.** On a peer link, the radio-style retry machinery is redundant. ALP runs over the stream with:
  - big windows, full duplex;
  - **content-hash resume** after a dropped connection;
  - an **end-to-end receipt** when the file or message is stored.

  Throughput is whatever TCP achieves. Nothing is tuned by hand.
- **Authentication:** both ends prove their call sign with a shared key per peer pair (HMAC-SHA256, the scheme Aether Term already uses with BBSes) or with keys exchanged over a trusted path. An unauthenticated peer can't inject traffic as another call sign. On the public internet the link also uses TLS.
- **NAT:** when neither side can accept connections, T2 is the answer. Hole punching (QUIC) is possible later but not proposed.

### 2.4 T4: amateur IP networks (AREDN, HamWAN)

- These are IP networks on amateur frequencies, often Mbps, built for emergency communications. Nodes have reachable addresses and DNS names (`call.local.mesh` on AREDN), so T3 peer links work directly, with no NAT.
- **Part 97 forbids encryption that obscures meaning** on amateur frequencies. On an amateur-radio network:
  - TLS is **off**;
  - authentication (HMAC) stays on, since it hides nothing;
  - the port is marked "amateur network", and Aether DV refuses to turn encryption on there.
- Service discovery through AREDN's advertised services is a later option.

### 2.5 Path choice and failover

- **What each peer can do:** Aether DV already plans a per-peer capability cache (ADR-0004), recording which paths reach that station and how fast. This extends it with the tiers above, with each entry ageing out.
- **Per message or file**, the engine picks the fastest path that works now: T3/T4 → T2 → T1/T0 → RF Fast Data → RF slow data. It falls back automatically when a path fails, and resumes by content hash, so nothing is re-sent from the start.
- **Operator policy per message or class:**
  - "RF only": for drills, or when internet paths must not be relied on;
  - "Any path" (the default);
  - "Internet only": never put large files on the air.
- **Shown everywhere:** each message and transfer says which path carried it ("via peer link, 1.2 MB/s"). Delivery is "delivered" only after the end-to-end receipt (`04` §4.1).
- **Bridging to RF stays deliberate:** a fast internet path never hands bulk data to a radio port automatically. The RF courtesy rules (`10` §4.4) apply to anything that goes on the air.

## 3. Regulatory and safety notes

- **Public internet links** aren't radio and carry no Part 97 obligations, but whatever is later retransmitted on RF does.
- **Amateur IP networks** (T4) are amateur radio: no encryption, station identification, no business traffic. The port type enforces the encryption rule.
- **Peer listening** is off by default, bound to an interface the operator picks, authenticated, and rate-limited.
- **No call-sign spoofing:** traffic from T2/T3/T4 peers is accepted only from authenticated call signs. Ratflector traffic stays unauthenticated, as in D-RATS, and is labelled so.

## 4. Plan, if adopted

| Step | Content | Test |
|---|---|---|
| F1 | T1: tuned legacy blocks and window on ratflector ports, measured round trip, per-ratflector courtesy cap, large-file confirmation | Against D-RATS 0.4.x through `aetherdv-rfd` (FI-1); throughput on LAN and internet |
| F2 | ALP over a byte stream: full duplex, big windows, content-hash resume, receipts (the subset of ADR-0004 that streams need) | Unit and simulation; resume after dropped connections |
| F3 | T2: unicast relay in `aetherdv-rfd`, capability exchange, per-client unicast rates | Server tests with Aether DV and D-RATS clients mixed |
| F4 | T3: peer listener, HMAC call-sign authentication, TLS on the public internet, address exchange through T2 | Two stations over the internet; failure and resume |
| F5 | T4: amateur-network port type (no TLS, enforced), AREDN DNS names | Bench on an AREDN node |
| F6 | Path choice and failover per destination, operator policies, path shown in the UI | Simulation: internet drops mid-transfer and the transfer continues on RF |

## 5. Validation register

| ID | Question |
|---|---|
| FI-1 | Largest block and window a D-RATS 0.4.x receiver handles correctly over TCP |
| FI-2 | Throughput of each tier on LAN, home broadband and mobile data |
| FI-3 | Resume behaviour when a peer link drops mid-file and the path changes |
| FI-4 | AREDN: reachability by DNS name, typical throughput, operator expectations about traffic volume |
| FI-5 | Load on a shared `aetherdv-rfd` with mixed unicast and flooded traffic |

## 6. Decisions for later

1. Adopt tiered IP paths, and in which order. Recommendation: F2 and F3 first (unicast relay and streaming ALP), since they help every Aether DV pair on a shared server without opening ports. Then F6, then T3/T4.
2. Peer authentication: per-pair shared keys (simple, like Aether Term's BBS HMAC), or public keys exchanged over a trusted path.
3. Whether T1 (faster legacy transfers through public ratflectors) is worth the load it puts on other stations, or whether large files should require T2 or better.
