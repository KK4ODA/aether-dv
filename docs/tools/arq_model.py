# Analytical half-duplex windowed selective-repeat ARQ model for D-STAR DV data.
# Purpose: make the assumptions behind slow/fast throughput claims explicit so
# they can be replaced with measured values in Phase C/D. NOT measured data.
import math

# ---- assumptions (all to be measured on hardware) ----
RATE = {"slow": 50 / 0.420,      # 10 x 5-byte slow-data blocks per 420 ms superframe = 119 B/s ceiling
        "fast": 3480 / 8}        # Icom-advertised 3480 bps -> 435 B/s ceiling
T_KEYUP = 0.60    # s: Icom documents ~0.5 s carrier sense before each data TX, plus key-up,
                  #    preamble, RF header, receiver lock (sum is a guess until HW-4)
T_TAIL = 0.50     # s: radio data-TX-end timer / tail before RX (guess; e.g. TH-D75 menu 614 = 0.5 s)
FORCE_DELAY = 1.25  # s: D-RATS default force_delay=-2 -> uniform 0.5..2.0 s random pre-TX wait
DDT2_FIXED = 25 + 10 + 11   # header + [SOB][EOB] + zlib wrapper on already-compressed data
YENC = 1.046                # measured expansion on incompressible bytes
ACK_FRAME = 55              # bytes on serial for ACK/REQACK frame incl. envelope


def over_time(nbytes, mode, legacy_delay=True):
    return (T_KEYUP + T_TAIL + (FORCE_DELAY if legacy_delay else 0.0)
            + nbytes / RATE[mode])


def goodput(mode, block, window, ber, legacy_delay=True):
    """Expected application bytes/s for a long transfer."""
    wire_block = (block + DDT2_FIXED) * YENC
    p_ok = (1 - ber) ** (8 * wire_block)          # independent bit errors, no FEC
    p_ack_ok = (1 - ber) ** (8 * ACK_FRAME * 2)   # REQACK + ACK must both survive
    # per round: send `window` blocks (fresh + retransmissions), then exchange ACK
    t_round = over_time(window * wire_block + ACK_FRAME, mode, legacy_delay) \
        + over_time(ACK_FRAME, mode, legacy_delay)
    delivered = window * block * p_ok * p_ack_ok
    return delivered / t_round


print("Model goodput in bytes/s (and as % of mode ceiling). Assumptions in source.\n")
for ber in (0.0, 1e-5, 1e-4, 3e-4):
    print(f"BER {ber:g}")
    for mode in ("slow", "fast"):
        row = []
        for block, window in ((512, 4), (512, 8), (256, 8), (128, 16), (1024, 4)):
            g = goodput(mode, block, window, ber)
            row.append(f"{block}x{window}: {g:6.1f} ({100*g/RATE[mode]:3.0f}%)")
        print(f"  {mode:4s} legacy  " + " | ".join(row))
    g_slow = goodput("slow", 512, 4, ber)
    g_fast = goodput("fast", 512, 4, ber)
    g_fast_tuned = max(goodput("fast", b, w, ber, legacy_delay=False)
                       for b in (64, 128, 256, 512, 1024) for w in (4, 8, 16, 32))
    g_slow_tuned = max(goodput("slow", b, w, ber, legacy_delay=False)
                       for b in (64, 128, 256, 512, 1024) for w in (4, 8, 16, 32))
    print(f"  D-RATS defaults 512x4: slow {g_slow:5.1f}  fast {g_fast:5.1f} B/s  (fast/slow {g_fast/g_slow:4.2f}x)")
    print(f"  tuned (no random delay, best block/window): slow {g_slow_tuned:5.1f}  fast {g_fast_tuned:5.1f} B/s "
          f"(tuning gain slow {g_slow_tuned/g_slow:4.2f}x, fast-tuned/slow-tuned {g_fast_tuned/g_slow_tuned:4.2f}x)\n")

# Short message latency (single over, no ACK), seconds
for name, n in (("chat 20 chars", 59), ("ICS-213 form ~1.2 KB zlib->~600 B, 2 blocks + session setup", 0)):
    pass
print("Single chat frame (59 B on serial) one-way air time:")
for mode in ("slow", "fast"):
    print(f"  {mode}: {over_time(59, mode):.2f} s (incl. {FORCE_DELAY}s mean random delay), "
          f"{over_time(59, mode, False):.2f} s without it")
