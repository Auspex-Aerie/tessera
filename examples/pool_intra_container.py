"""Intra-container Pool demo: producer + worker subprocess sharing one SHM region.

Topology T1 ("Intra-container") per the Tessera design — owner process
+ worker subprocesses inside the same container. The owner creates a
Pool, hands each chunk's Descriptor to a worker through an IPC
channel, and the worker reads the payload bytes from shared memory
(no copy through pickle).

Run from the workspace root:
    python examples/pool_intra_container.py

Requires ``tessera-pool`` installed in the active venv (``maturin
develop`` from ``python/py-tessera-pool/``).
"""

from __future__ import annotations

import multiprocessing as mp
import os
import time

from tessera_pool import Descriptor, Pool


def worker_main(description: str, descriptors_in: "mp.Queue[Descriptor | None]") -> None:
    """Subprocess entrypoint: attach to the SHM region by description, read
    each descriptor's payload, exit on receiving the sentinel ``None``."""
    # Attach with the same geometry the owner used. ttl_seconds is
    # ignored on attach (inherited from the SHM header).
    pool = Pool(
        description=description,
        slot_count=4,
        slot_size_bytes=1024 * 1024,
        is_owner=False,
    )
    received = 0
    total_bytes = 0
    while True:
        descriptor = descriptors_in.get()
        if descriptor is None:
            break
        payload = pool.read_payload(descriptor)
        received += 1
        total_bytes += len(payload)
        # In real code, the worker would write the payload to disk,
        # forward it to a downstream service, etc. Here we just count.
    print(f"  worker[{os.getpid()}]: drained {received} descriptors, {total_bytes:,} bytes")


def main() -> None:
    description = f"tessera-example/pool-intra/{os.getpid()}"
    descriptors_q: "mp.Queue[Descriptor | None]" = mp.Queue()

    worker = mp.Process(
        target=worker_main,
        args=(description, descriptors_q),
        daemon=True,
    )
    worker.start()
    # Give the worker a moment to attach. In real code, an explicit
    # "ready" handshake would replace this sleep.
    time.sleep(0.1)

    with Pool(
        description=description,
        slot_count=4,
        slot_size_bytes=1024 * 1024,
        ttl_seconds=30.0,
    ) as pool:
        print(f"owner[{os.getpid()}]: created {pool}")
        for i in range(6):
            payload = f"batch-{i:03d}".encode() * 4096  # ~32 KB each
            lease = pool.acquire(timeout_seconds=2.0)
            descriptor = pool.write(lease, payload)
            descriptors_q.put(descriptor)
            pool.release(lease)
            print(f"  owner: handed off batch-{i:03d} ({len(payload):,} bytes)")

        # Sentinel — worker exits on this.
        descriptors_q.put(None)

    worker.join(timeout=5.0)
    if worker.is_alive():
        worker.terminate()
        raise SystemExit("worker did not exit cleanly")


if __name__ == "__main__":
    main()
