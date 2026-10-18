"""A bounded presentation tail; dropping these handles never drops decode input."""

from collections import deque


class ReadySurfaces:
    def __init__(self):
        self.frames = deque(maxlen=4)
        self.generation = 0
        self.published = 0
        self.superseded = 0
        self.not_ready = 0
        self.taken = 0

    def add(self, frame):
        if frame.generation < self.generation:
            return
        if frame.generation != self.generation:
            self.superseded += len(self.frames)
            self.frames.clear()
            self.generation = frame.generation
        if len(self.frames) == self.frames.maxlen:
            self.superseded += 1
        self.frames.append(frame)
        self.published += 1

    def take(self):
        ready = next(
            (i for i in range(len(self.frames) - 1, -1, -1) if self.frames[i].ready),
            None,
        )
        if ready is None:
            self.not_ready += bool(self.frames)
            return None
        frame = self.frames[ready]
        self.superseded += ready
        self.taken += 1
        for _ in range(ready + 1):
            self.frames.popleft()
        return frame
