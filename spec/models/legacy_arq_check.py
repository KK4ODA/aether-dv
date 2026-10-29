#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Explicit-state model checker for the D-RATS stateful-session ARQ.

Models the sender and receiver logic of D-Rats `d_rats/sessions/stateful.py`
(commit d0e7a39) over a lossy FIFO channel and explores every reachable
state breadth-first. Sequence numbers are reduced modulo MOD (legacy: 256)
so that wrap-around is reachable in a small state space; the legacy wrap
test `seq == 0 and iseq == 255` becomes `seq == 0 and iseq == MOD - 1`.

Faithfully modelled (line numbers refer to stateful.py@d0e7a39):
  * sender fills `outstanding` up to the window limit, pausing at the
    rollover boundary when block seq 0 would join a non-empty window (:173-178)
  * a send round transmits every outstanding block then one REQACK listing
    their seqs (:289-308); if still waiting for an ACK it re-sends only the
    REQACK (:274-286)
  * an ACK removes the listed blocks from `outstanding` and clears the wait
    (:375-392)
  * the receiver resets `recv_list` only on (seq 0 after iseq MOD-1) (:397-401),
    stores unseen seqs, answers REQACK with the subset in `recv_list`
    (:409-426), and releases blocks in order (:441-447)
Abstracted: timers (a resend may fire at any time, i.e. arbitrary delay),
the adaptive window (fixed W), retry limit, byte contents (block identity is
its global index).

Checked properties:
  P1 in-order, no-duplicate, correct delivery
  P2 no false acknowledgement: every block the sender considers ACKed is
     held by the receiver (delivered, or buffered under the right index)
  P3 when the sender finishes, the receiver has delivered everything

Receiver variants: `legacy` (as D-RATS) and `windowed` (proposed Aether
receiver: duplicate detection relative to the in-order point, valid while
window <= MOD/2; legacy senders keep the window far below 128).
Analysis aid; not production code.
"""
import argparse
from collections import deque


def check(mod, nblocks, window, chan_cap, receiver, sender_dup0=1, span_limit=False, max_states=5_000_000):
    # State:
    # (next_idx, outstanding(tuple idx), waiting(tuple seqs)|None, acked(frozenset idx),
    #  recv_list(frozenset seqs), oob(tuple (seq,idx) sorted), iseq, delivered,
    #  c_fwd(tuple msgs), c_rev(tuple msgs))
    init = (0, (), None, frozenset(), frozenset(), (), -1, 0, (), ())
    seen = {init: None}
    q = deque([init])

    def seqof(i):
        return i % mod

    def violation(state, why):
        # rebuild the trace
        trace = []
        s = state
        while s is not None:
            prev = seen[s]
            if prev is None:
                trace.append(("init", s))
                break
            trace.append((prev[1], s))
            s = prev[0]
        trace.reverse()
        return why, trace

    def rx_deliver(recv_list, oob, iseq, delivered):
        oob = dict(oob)
        while (iseq + 1) % mod in oob:
            idx = oob.pop((iseq + 1) % mod)
            if idx != delivered:
                return None, ("P1 wrong block delivered: expected #%d got #%d" % (delivered, idx))
            delivered += 1
            iseq = (iseq + 1) % mod
        return (recv_list, tuple(sorted(oob.items())), iseq, delivered), None

    def holds(seq, recv_list, iseq, oob, delivered):
        if receiver == "legacy":
            return seq in recv_list
        # windowed: held iff buffered, or among the last min(mod/2, delivered)
        # blocks delivered in order (distance measured back from iseq)
        if seq in dict(oob):
            return True
        back = (iseq - seq) % mod
        return back < min(mod // 2, delivered)

    def is_new(seq, recv_list, iseq, oob, delivered):
        if receiver == "legacy":
            return seq not in recv_list
        fwd = (seq - iseq) % mod
        return 1 <= fwd <= mod // 2 and not holds(seq, recv_list, iseq, oob, delivered)

    while q:
        st = q.popleft()
        (nxt, outst, waiting, acked, rl, oob, iseq, dlv, cf, cr) = st

        # P2: every acked block is held by the receiver
        oobd = dict(oob)
        for i in acked:
            if i >= dlv and oobd.get(seqof(i)) != i:
                return violation(st, "P2 false ACK: block #%d (seq %d) ACKed but receiver never got it" % (i, seqof(i))), len(seen)
        # P3
        if nxt == nblocks and not outst:
            # sender believes the transfer is complete
            missing = [i for i in range(dlv, nblocks) if oobd.get(seqof(i)) != i]
            if missing:
                return violation(st, "P3 sender finished but receiver lacks blocks %s" % missing), len(seen)

        succ = []
        # --- sender: send round (timeout may fire any time) ---
        if waiting is not None and outst:
            if len(cf) < chan_cap:
                succ.append(("TX REQACK %s (re-poll)" % list(waiting),
                             (nxt, outst, waiting, acked, rl, oob, iseq, dlv, cf + (("REQ", waiting),), cr)))
        else:
            o = list(outst)
            n = nxt
            while len(o) < window and n < nblocks:
                if seqof(n) == 0 and o and not span_limit:
                    break  # legacy rollover pause
                if span_limit and o and n - min(o) >= mod // 2:
                    break  # ALP rule: oldest-unacked to newest span < mod/2
                o.append(n)
                n += 1
            if o:
                msgs = []
                for i in o:
                    copies = sender_dup0 if (seqof(i) == 0 and i > 0) else 1
                    msgs.extend([("DAT", seqof(i), i)] * copies)
                msgs.append(("REQ", tuple(seqof(i) for i in o)))
                if len(cf) + len(msgs) <= chan_cap:
                    succ.append(("TX burst %s + REQACK" % ["#%d/s%d" % (i, seqof(i)) for i in o],
                                 (n, tuple(o), tuple(seqof(i) for i in o), acked, rl, oob, iseq, dlv, cf + tuple(msgs), cr)))
        # --- sender: receive ACK ---
        if cr:
            m = cr[0]
            acks = set(m[1])
            newly = tuple(i for i in outst if seqof(i) in acks)
            rest = tuple(i for i in outst if seqof(i) not in acks)
            succ.append(("RX ACK %s" % sorted(acks),
                         (nxt, rest, None, acked | frozenset(newly), rl, oob, iseq, dlv, cf, cr[1:])))
        # --- receiver ---
        if cf:
            m = cf[0]
            if m[0] == "DAT":
                _, s, idx = m
                rl2 = rl
                if receiver == "legacy" and s == 0 and iseq == mod - 1:
                    rl2 = frozenset()
                oob2 = oob
                if is_new(s, rl2, iseq, oob, dlv):
                    rl2 = rl2 | {s}
                    oob2 = tuple(sorted({**dict(oob), s: idx}.items()))
                    label = "RX DAT #%d/s%d stored" % (idx, s)
                else:
                    label = "RX DAT #%d/s%d DISCARDED as duplicate" % (idx, s)
                res, err = rx_deliver(rl2, oob2, iseq, dlv)
                if err:
                    return violation(st, err), len(seen)
                rl3, oob3, iseq3, dlv3 = res
                succ.append((label, (nxt, outst, waiting, acked, rl3, oob3, iseq3, dlv3, cf[1:], cr)))
            else:
                seqs = m[1]
                ack = tuple(s for s in seqs if holds(s, rl, iseq, oob, dlv))
                if len(cr) < chan_cap:
                    succ.append(("RX REQACK %s -> TX ACK %s" % (list(seqs), list(ack)),
                                 (nxt, outst, waiting, acked, rl, oob, iseq, dlv, cf[1:], cr + (("ACK", ack),))))
        # --- channel loss (any frame in flight) ---
        for k in range(len(cf)):
            m = cf[k]
            succ.append((("LOSS fwd DAT #%d/s%d" % (m[2], m[1])) if m[0] == "DAT" else "LOSS fwd REQACK",
                         (nxt, outst, waiting, acked, rl, oob, iseq, dlv, cf[:k] + cf[k + 1:], cr)))
        for k in range(len(cr)):
            succ.append(("LOSS rev ACK", (nxt, outst, waiting, acked, rl, oob, iseq, dlv, cf, cr[:k] + cr[k + 1:])))

        for label, s2 in succ:
            if s2 not in seen:
                seen[s2] = (st, label)
                q.append(s2)
                if len(seen) > max_states:
                    return ("state bound reached", []), len(seen)
    return None, len(seen)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mod", type=int, default=4)
    ap.add_argument("--blocks", type=int, default=6)
    ap.add_argument("--window", type=int, default=2)
    ap.add_argument("--chan", type=int, default=4)
    ap.add_argument("--receiver", choices=["legacy", "windowed"], default="legacy")
    ap.add_argument("--dup0", type=int, default=1, help="copies of the first post-wrap block (sender mitigation)")
    ap.add_argument("--span-limit", action="store_true", help="ALP sender rule instead of legacy rollover pause")
    a = ap.parse_args()
    res, n = check(a.mod, a.blocks, a.window, a.chan, a.receiver, a.dup0, a.span_limit)
    print(f"config: mod={a.mod} blocks={a.blocks} window={a.window} chan={a.chan} receiver={a.receiver} dup0={a.dup0} span_limit={a.span_limit}")
    print(f"states explored: {n}")
    if res is None:
        print("RESULT: no violation of P1/P2/P3 in the full reachable state space")
        return 0
    why, trace = res
    if not trace:
        print("RESULT: INCONCLUSIVE —", why)
        return 2
    print("RESULT: VIOLATION —", why)
    print("shortest counterexample:")
    for step, (label, _s) in enumerate(trace):
        print(f"  {step:2d}. {label}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
