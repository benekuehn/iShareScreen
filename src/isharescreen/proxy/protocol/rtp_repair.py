"""Bounded, thread-safe NACK retries inside the access-unit repair window."""

from collections import Counter
from dataclasses import dataclass
import threading


@dataclass
class Missing:
    first_seen: float
    next_request: float
    attempts: int = 0


class PacketRepair:
    def __init__(
        self, *, window=0.075, retry=0.020, attempts=3, packet_cap=1024, source_cap=32
    ):
        self.window, self.retry, self.attempts = window, retry, attempts
        self.packet_cap, self.source_cap = packet_cap, source_cap
        self._sources = {}
        self._counts = Counter()
        self._lock = threading.Lock()

    def missing(self, ssrc, sequences, now):
        with self._lock:
            self._expire(now)
            if ssrc not in self._sources and len(self._sources) >= self.source_cap:
                # Prefer current streams without retaining an unbounded history
                # of SSRC generations. Dict insertion order is first-seen order.
                oldest = next(iter(self._sources))
                self._counts["evicted"] += len(self._sources.pop(oldest))
            packets = self._sources.setdefault(ssrc, {})
            for seq in sequences:
                if seq in packets:
                    continue
                if len(packets) >= self.packet_cap:
                    packets.pop(next(iter(packets)))
                    self._counts["evicted"] += 1
                packets[seq] = Missing(now, now)
                self._counts["missing"] += 1

    def received(self, ssrc, seq, now):
        with self._lock:
            packets = self._sources.get(ssrc)
            item = packets.pop(seq, None) if packets else None
            if item is not None:
                if now - item.first_seen >= self.window:
                    self._counts["expired"] += 1
                    self._counts["returned_too_late"] += 1
                else:
                    self._counts["returned"] += 1
                    # Reordering can also return a packet after a NACK. This
                    # counter alone does not prove a server retransmission.
                    self._counts["returned_after_nack"] += bool(item.attempts)
            if packets is not None and not packets:
                self._sources.pop(ssrc, None)

    def _expire(self, now):
        for source, packets in list(self._sources.items()):
            for seq, item in list(packets.items()):
                if now - item.first_seen >= self.window:
                    del packets[seq]
                    self._counts["expired"] += 1
            if not packets:
                del self._sources[source]

    def due(self, now):
        """Reserve due requests atomically; callers send outside the lock."""
        with self._lock:
            self._expire(now)
            requests = []
            for source, packets in self._sources.items():
                seqs = []
                for seq, item in packets.items():
                    if item.attempts < self.attempts and now >= item.next_request:
                        item.attempts += 1
                        item.next_request = now + self.retry
                        seqs.append(seq)
                if seqs:
                    requests.append((source, seqs))
                    self._counts["nack_datagrams_attempted"] += 1
                    self._counts["nack_sequences_attempted"] += len(seqs)
            return requests

    def send_failed(self):
        with self._lock:
            self._counts["nack_send_errors"] += 1

    def next_delay(self, now, maximum):
        with self._lock:
            self._expire(now)
            deadline = now + maximum
            for packets in self._sources.values():
                for item in packets.values():
                    deadline = min(deadline, item.first_seen + self.window)
                    if item.attempts < self.attempts:
                        deadline = min(deadline, item.next_request)
            return max(0, deadline - now)

    def snapshot(self):
        with self._lock:
            return {
                **dict(self._counts),
                "pending": sum(len(packets) for packets in self._sources.values()),
            }
