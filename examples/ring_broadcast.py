"""Multi-reader broadcast demo: two subprocess consumers, each sees every event.

This example highlights the distinctive Ring property vs Pool: every
reader handle maintains its own process-local cursor, so multiple
consumers attached to the same Ring section see the full event stream
independently. There is no fanout coordinator — the seqlock-protected
slot reads scale to N consumers with no extra writer-side work.

Use cases this enables:
  - A live TUI display reading the same metric stream that a Prometheus
    exporter is also draining.
  - A log archiver (durable sink) alongside an alert classifier
    (latency-sensitive filter) reading the same log Ring.
  - A debugging sidecar attached at runtime that observes traffic
    without affecting production consumers.

Run from the workspace root:
    python examples/ring_broadcast.py

Requires ``tessera-ring`` installed in the active venv (``maturin
develop`` from ``python/py-tessera-ring/``).
"""

from __future__ import annotations

import multiprocessing as mp
import os
import time
from typing import List

from tessera_ring import Ring


N_SLOTS = 16
SLOT_SIZE = 256
N_PUBLISHES = 12
SECTION_ID = 0


def consumer_main(
    name: str,
    description: str,
    ready: "mp.Event",
    done: "mp.Event",
    poll_interval_seconds: float,
    out_queue: "mp.Queue[List[tuple[int, bytes]]]",
) -> None:
    """Subprocess: attach to the Ring, drain Section 0 with the configured
    poll cadence, push the full delivered sequence back to the owner for
    cross-consumer comparison."""
    with Ring(
        description=description,
        sections=[(SECTION_ID, N_SLOTS, SLOT_SIZE)],
        is_owner=False,
    ) as ring:
        reader = ring.reader(SECTION_ID)
        ready.set()

        delivered: List[tuple[int, bytes]] = []
        while True:
            events = reader.poll()
            for e in events:
                delivered.append((e.position, e.payload))
            if not events:
                if done.is_set():
                    break
                time.sleep(poll_interval_seconds)

        stats = reader.stats()
        print(
            f"  {name}[{os.getpid()}]: drained {len(delivered)} events; "
            f"final stats: cursor={stats.cursor} latest={stats.latest} dropped={stats.dropped}"
        )
        out_queue.put(delivered)


def main() -> None:
    description = f"tessera-example/ring-broadcast/{os.getpid()}"
    ready_fast = mp.Event()
    ready_slow = mp.Event()
    done = mp.Event()
    fast_out: "mp.Queue[List[tuple[int, bytes]]]" = mp.Queue()
    slow_out: "mp.Queue[List[tuple[int, bytes]]]" = mp.Queue()

    with Ring(
        description=description,
        sections=[(SECTION_ID, N_SLOTS, SLOT_SIZE)],
    ) as ring:
        print(f"owner[{os.getpid()}]: created Ring (slots={N_SLOTS}, size={SLOT_SIZE})")

        # Two consumers with different poll cadences. The fast consumer
        # keeps up event-for-event; the slow consumer falls behind but
        # — because Ring is multi-reader broadcast, NOT work-distribution
        # — STILL receives every event the writer publishes (within the
        # ring's capacity).
        fast = mp.Process(
            target=consumer_main,
            args=("fast-consumer", description, ready_fast, done, 0.001, fast_out),
            daemon=True,
            name="fast-consumer",
        )
        slow = mp.Process(
            target=consumer_main,
            args=("slow-consumer", description, ready_slow, done, 0.020, slow_out),
            daemon=True,
            name="slow-consumer",
        )
        fast.start()
        slow.start()

        for ev in (ready_fast, ready_slow):
            if not ev.wait(timeout=5.0):
                raise SystemExit("consumer did not attach within 5s")

        writer = ring.writer()
        for i in range(N_PUBLISHES):
            payload = f"event-{i:03d}".encode()
            writer.publish(SECTION_ID, payload)
            time.sleep(0.005)

        # Let both consumers catch up, then signal done.
        time.sleep(0.1)
        done.set()
        for proc in (fast, slow):
            proc.join(timeout=5.0)
            if proc.is_alive():
                proc.terminate()
                raise SystemExit(f"{proc.name} did not exit cleanly")

        fast_seq = fast_out.get(timeout=1.0)
        slow_seq = slow_out.get(timeout=1.0)

        # Both consumers must have seen identical event sequences.
        # Order is preserved (writers fetch-add a monotonic global
        # position); payload bytes match.
        assert fast_seq == slow_seq, (
            "broadcast violation: fast and slow consumers diverged "
            f"({len(fast_seq)} vs {len(slow_seq)} events)"
        )
        assert len(fast_seq) == N_PUBLISHES, (
            f"expected {N_PUBLISHES} events per consumer, got {len(fast_seq)} "
            "(consider widening the ring or extending the catch-up sleep)"
        )
        for i, (position, payload) in enumerate(fast_seq):
            assert position == i, f"position drift at index {i}: got {position}"
            assert payload == f"event-{i:03d}".encode()

        print(
            f"owner: broadcast verified — both consumers drained "
            f"{len(fast_seq)} identical events from the same Ring section"
        )


if __name__ == "__main__":
    main()
