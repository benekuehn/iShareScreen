"""Authenticated RTP reception statistics for RTCP RR (RFC 3550 A.3/A.8).

SRTP authentication remains with the session. Sequence discontinuities use
RFC 3550 A.1's dropout/misorder bounds and two-packet restart confirmation.
Arrival times are observed at packet processing, so jitter includes local
scheduling delay as well as network variation. No payload data is retained.
"""

import threading

# RFC 3550's example uses 100 packets of misorder at much lower packet rates.
# A 4K picture alone can span hundreds; allow the bounded 1,024-packet NACK
# window to arrive late without falsely classifying a repair as a restart.
MAX_DROPOUT = 3000
MAX_MISORDER = 4096


class ReceptionStats:
    def __init__(self, clock_rate=90000):
        self.clock_rate = clock_rate
        self._lock = threading.Lock()
        self.base = self.highest = None
        self.received = 0
        self.expected_prior = self.received_prior = 0
        self.arrival = self.timestamp = None
        self.jitter = 0.0
        self.bad_seq = 65537

    def _restart(self, seq):
        self.base = self.highest = seq
        self.received = self.expected_prior = self.received_prior = 0
        self.arrival = self.timestamp = None
        self.jitter = 0.0
        self.bad_seq = 65537

    def observe(self, seq, timestamp, arrival):
        with self._lock:
            if self.highest is None:
                # The caller has already authenticated this source; begin
                # accounting immediately rather than requiring probation.
                self._restart(seq)
            else:
                delta = (seq - (self.highest & 0xFFFF)) & 0xFFFF
                if delta < MAX_DROPOUT:
                    self.highest += delta
                elif delta <= 65536 - MAX_MISORDER:
                    if seq == self.bad_seq:
                        self._restart(seq)
                    else:
                        self.bad_seq = (seq + 1) & 0xFFFF
                        return  # unconfirmed discontinuity, not massive loss
                # Otherwise this is a duplicate/late packet near the frontier.
            # RFC 3550 counts late and duplicate packets as received; loss
            # can therefore be negative. Report intervals clamp that to 0.
            self.received += 1
            if self.arrival is not None:
                ticks = (
                    (timestamp - self.timestamp + 0x80000000) & 0xFFFFFFFF
                ) - 0x80000000
                difference = (arrival - self.arrival) * self.clock_rate - ticks
                self.jitter += (abs(difference) - self.jitter) / 16
            self.arrival, self.timestamp = arrival, timestamp

    def report(self):
        with self._lock:
            if self.highest is None:
                return {}
            expected = self.highest - self.base + 1
            interval = expected - self.expected_prior
            lost_interval = interval - (self.received - self.received_prior)
            fraction = (
                min(255, (lost_interval << 8) // interval)
                if interval > 0 and lost_interval > 0
                else 0
            )
            self.expected_prior, self.received_prior = expected, self.received
            return {
                "extended_seq": self.highest & 0xFFFFFFFF,
                "lost": max(-0x800000, min(0x7FFFFF, expected - self.received)),
                "fraction_lost": fraction,
                "jitter": min(0xFFFFFFFF, int(self.jitter)),
            }
