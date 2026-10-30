# 13 — Resilient servers and store-and-forward relays: design note

Date: 2026-10-30 · Status: **proposal for a later decision; nothing implemented**

Evidence tags as in `03`. Builds on:
- `04` (ratflector analysis: flooding hub, link loops, duplicate suppression, RF↔internet bridging)
- `12` (fast IP paths: unicast relay, path choice, failover)
- ADR-0004 (ALP: receipts, content-hash resume)

Source read: D-Rats `d0e7a39` `msgrouting.py`.

## 0. Summary

Two failure modes matter in emergency use:
1. **The server's site fails** (power, internet). Today a ratflector usually runs on one PC or Raspberry Pi; when that site goes down, every station on it is cut off.
2. **A station's site loses internet** but still has a radio. Its traffic should move to RF, directly or through a station that still has both.

Two proposals:
- **A ratflector cluster.** Several `aetherdv-rfd` instances in different places (two cloud providers, optionally an EOC Pi) linked into one logical server. Clients fail over between instances, and instances share who is connected where.
- **Store-and-forward relays.** Aether DV stations with both internet and RF carry *messages* (forms, files, ICS-213) for stations that lost one path. They learn reachability over the internet, protect RF capacity, and confirm delivery end to end.

The cluster doesn't help a station whose own internet is down; the relays do. Together they cover both failures.

## 1. Ratflector cluster

### 1.1 Why it's easy

A ratflector keeps no data (`04` §1): it forwards frames between connected clients. A cluster needs no shared database, only:
- links between instances;
- loop suppression;
- a shared view of who is connected where.

### 1.2 Design

- **Instances.** Run 2-3 `aetherdv-rfd` instances on different providers or regions. A site instance (Pi at an EOC) can join the cluster. It keeps serving local clients if its internet fails, and can bridge to RF (§2).
- **Linking.** Each instance holds a link to every other (full mesh; clusters are small). Frames from a client go to the instance's other clients and to the other instances, which pass them only to their own clients, never to another instance.
- **Loop protection, in two layers:**
  - **The forwarding rule:** instance-to-instance frames are never re-forwarded to other instances.
  - **Cross-link duplicate suppression** from `04` §5: per link, a short window (~3 s on internet-only links). It catches frames arriving twice by different routes, for example a client connected to two instances.
- **Presence.** Each instance announces its connected call signs (and whether each is D-RATS or Aether DV) to the others. That gives:
  - **"who's here"** across the whole cluster;
  - **unicast delivery** (`12` T2): a frame for KK4ODA goes only to the instance KK4ODA is on.
- **Clients:**
  - **Aether DV** takes a list of servers for one port. It connects to the first that answers and switches on failure. Optionally it stays connected to two and drops duplicate frames, so a failover loses nothing.
  - **D-RATS clients** get one host name with several addresses (DNS), plus health-checked DNS if the operator has it. They reconnect to whichever instance answers.
- **Rate limits** stay per client (`aetherdv-rfd` defaults 64 KiB/s, 256 KiB burst). Instance-to-instance links get a higher limit so they don't throttle each other.
- **Authentication between instances:** a shared cluster key (HMAC), so outsiders can't join the cluster or inject presence. Client access is as today, with an optional password.
- **Packaging:** a container image of `aetherdv-rfd` with a health endpoint, a sample cluster config and monitoring hints. Small cloud machines handle hundreds of stations at the per-client limits [MODEL].

### 1.3 Limits

- **Public internet only.** Over AREDN/HamWAN, encryption rules apply (`12` §2.4).
- **The cloud keeps the server up, not the clients.** A station with no internet needs §2.
- **The service is one logical server.** All instances should be run by the same group, with one configuration.

## 2. Store-and-forward relays

### 2.1 What D-RATS does today

`msgrouting.py` routes forms and messages, not frames, one hop at a time [SRC]:

| Rule | Code |
|---|---|
| Send directly if the destination was heard within `station_msg_ttl` | `_route_msg`, `msgrouting.py:463-555` |
| Else a static route from `routes.txt` (`dest gateway port`), else the default route `*` | `_get_routes`, `:373-400` |
| Explicit source route `A;B;C`, followed hop by hop | `gratuitous_next_hop`, `:134-165` |
| Never route to a station already in the message's path; skip stations marked failed; ping stale routes before using them | `_route_msg` |

That's a store-and-forward mesh. Its limits:
- routes are static or "heard recently on some port";
- nothing reports reachability between stations;
- there's no end-to-end receipt (a D-RATS hop is done when the next station accepts);
- there's no protection for a slow RF link.

### 2.2 Design

- **What is relayed: messages, not frames.**
  - Forms, ICS-213, files and stored messages are relayed, each with a unique message id. Duplicates are discarded by id.
  - **Live chat isn't relayed;** it stays on whatever path the sender has.
  - Frame-level bridging (a ratflector owning a radio) stays possible, but isn't how relays work (`04` §4.1 explains why).
- **Relays.**
  - Any Aether DV station can offer to relay (off by default). A relay is typically one with internet and RF that's well placed, for example a hospital or EOC with a good antenna.
  - An operator setting decides **what** it relays: precedence classes, maximum size per message, total per hour on RF.
- **Learning who can reach whom.**
  - Each station keeps its **heard table** (per port: call sign, last heard, link quality) as today.
  - Relays **share theirs over the internet** (the cluster, or `12` T3 peer links): "I hear EOC on RF via W4DOC B, 2 min ago".
  - **Nothing extra is beaconed on RF.** On RF, reachability comes only from traffic already heard.
  - Static routes stay available, D-RATS style, and take precedence when set.
- **Choosing a route,** per message, in order:
  1. a direct path that works now (`12` §2.5);
  2. a learned relay with a fresh heard entry for the destination, preferring the most recent and the best link;
  3. a static route;
  4. otherwise **hold** the message in the outbox, marked "waiting for a route", and retry when reachability changes.

  The route is shown on the message ("via HOSPITAL on RF").
- **Delivery and receipts.**
  - Every hop is acknowledged, as in D-RATS.
  - The final recipient sends an **end-to-end receipt** back along any available path.
  - The sender's message shows "Delivered" only after that receipt.
  - A relay holding a message shows it under "Relaying"; its owner can see and cancel it.
- **Protecting RF.** The relay applies RF rules to everything it puts on the air:
  - **precedence order:** Flash / Immediate / Priority / Routine (ICS-213), then age;
  - **size limits:** large files wait for the internet, or need the relay operator's approval;
  - the repeater or simplex **courtesy rules** (`10` §4.4): air-time budget, voice yield.

  The result: an outage doesn't turn the internet's backlog into a flood on a shared repeater.
- **No loops:** a hop limit (default 4), the message's path list (as D-RATS), and per-message ids remembered by every relay.
- **Example: the EOC loses internet.**
  1. **The county** sends a form to the EOC through the cluster.
  2. **The cluster** sees the EOC disconnected. The hospital relay has reported "EOC heard on RF 2 min ago".
  3. **The hospital** stores the form and sends it over W4DOC B under the courtesy rules.
  4. **The EOC's** receipt goes back to the hospital on RF, then over the internet to the county. The county sees "Delivered via HOSPITAL (RF)".
  5. **In the other direction,** a form from the EOC to an internet-only destination goes by RF to the hospital, addressed through it, and the hospital forwards it over the internet.
- **D-RATS stations:**
  - can be the **last hop**: a relay delivers with the legacy form transfer, without an end-to-end receipt (marked "handed to D-RATS station");
  - can't act as relays in this scheme;
  - their own `routes.txt` routing keeps working alongside.

### 2.3 Safety and policy

- **Operators stay in charge.** Relaying is off by default; what and how much is the relay operator's setting; every relayed message is visible and cancellable at the relay.
- **Third-party and gateway rules:** messages relayed from the internet onto amateur RF must come from licensed stations. Relays accept only traffic from authenticated call signs (cluster or peer-link authentication, `12` §2.3) for RF delivery.
- **Privacy:** relays see message contents, as in D-RATS and Winlink, since amateur RF can't be encrypted. The setup text says so.

## 3. Plan, if adopted

| Step | Content | Depends on | Test |
|---|---|---|---|
| R1 | Client list of servers with failover (and optional dual connection with duplicate dropping) | — | Kill an instance mid-chat; no loss with dual connection |
| R2 | Instance-to-instance links, forwarding rule, cross-link duplicate suppression, cluster key | `04` §5 rules | Simulation: 3 instances, clients on each, loops and partitions |
| R3 | Cluster presence; unicast delivery across instances | R2, `12` F3 | Mixed D-RATS and Aether DV clients |
| R4 | Container image, health endpoint, sample cluster configuration, monitoring notes | R2 | Deploy two cloud instances plus a Pi |
| R5 | Store-and-forward core: message ids, relay setting, outbox "waiting for a route", hop limit, path list | ALP receipts (ADR-0004) | Simulation with RF and internet ports |
| R6 | Reachability sharing over the internet; route choice; static routes | R5, R3 | Simulation: internet loss at one site; messages rerouted via relay |
| R7 | RF protection on relays: precedence, size limits, air-time budget; UI for "Relaying" and routes on messages | R5 | Simulation plus an on-air drill with a trustee |

## 4. Validation register

| ID | Question |
|---|---|
| RS-1 | Failover time for Aether DV and for D-RATS clients (DNS) when an instance dies |
| RS-2 | Duplicate rate and loop safety in a 3-instance cluster with dual-connected clients |
| RS-3 | Relay throughput limits on a shared repeater during a simulated outage backlog |
| RS-4 | How long heard entries stay useful for routing on RF (fixed stations vs mobiles) |
| RS-5 | D-RATS behaviour when receiving relayed forms (last hop), and interplay with its own `routes.txt` |

## 5. Decisions for later

1. Adopt the cluster, relays, or both, and in which order. Recommendation:
   - **R1-R2 first:** cheap, and they help D-RATS users too;
   - **R5-R7 next:** the outage value;
   - **R3 with `12` T2.**
2. Who runs the cloud instances, and whether the project publishes a reference cluster.
3. The default relay limits (per message and per hour on RF), to agree with local ARES/EmComm groups.
