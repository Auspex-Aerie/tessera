"""Tessera Ring — lossy mmap-backed multi-writer / multi-reader ring buffer.

Thin Python facade over the Rust core in ``tessera-ring``. The native
extension module (``tessera_ring._native``) provides the implementation;
this package re-exports the public surface for ergonomic import.

```python
from tessera_ring import Ring

with Ring(description="my-app/telemetry",
          sections=[(0, 4096, 2048)]) as ring:
    writer = ring.writer()
    reader = ring.reader(0)
    writer.publish(0, b"hello")
    for event in reader.poll():
        print(event.position, event.payload)
```

Public symbols:

- ``Ring``: the ring itself; context-manager-friendly.
- ``Writer``: handle for publishing events.
- ``Reader``: handle for draining events from one section.
- ``Event``: frozen result of ``Reader.poll()``.
- ``ReaderStats``: frozen result of ``Reader.stats()``.
- ``TesseraRingError``: base exception class for all ring errors.
"""

from tessera_ring._native import (
    Event,
    Reader,
    ReaderStats,
    Ring,
    TesseraRingError,
    Writer,
    _event_from_parts,
)

__version__ = "0.0.1"
__all__ = ["Event", "Reader", "ReaderStats", "Ring", "TesseraRingError", "Writer"]
