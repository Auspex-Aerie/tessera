"""Tessera Sink — atomic-write worker pool to disk.

Thin Python facade over the Rust core in ``tessera-sink``. The native
extension module (``tessera_sink._native``) provides the implementation;
this package re-exports the public surface for ergonomic import.

```python
from tessera_sink import Sink

with Sink(description="my-app/artifacts",
          worker_count=4,
          pool_slot_count=8,
          pool_slot_size_bytes=64 * 1024 * 1024) as sink:
    sink.submit("/data/out.parquet", payload_bytes, fsync=True)
    sink.flush()
```

The caller hands ``submit`` pre-serialized ``bytes``; chunking, hashing,
atomic temp+rename all happen in the Rust core / worker subprocesses.

Public symbols:

- ``Sink``: context-manager-friendly Sink class.
- ``TesseraSinkError``: base exception class for all Sink errors.
"""

from tessera_sink._native import Sink, TesseraSinkError

__version__ = "0.0.1"
__all__ = ["Sink", "TesseraSinkError"]
