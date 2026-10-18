#!/usr/bin/env python3
"""Offline GPU validation. Readback is used only here, never in the viewer.

Input: an Annex-B HEVC 4:4:4 stream, e.g. the command in native/README.md.
Reports completed decode + composition + GPU draw time (glFinish), rather
than measuring only asynchronous GL submission. No remote desktop required.
This does not measure compositor presentation or display scanout.
"""

import argparse
import ctypes as C
import json
import os
import time

import av
import glfw
import numpy as np

from isharescreen.frontend.native.app import Renderer, create_window
from isharescreen.proxy.media.native import NativeContext

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("stream")
parser.add_argument("--frames", type=int, default=180)
args = parser.parse_args()
os.environ["ISS_NATIVE_GPU"] = "1"
gl = C.CDLL("libGLESv2.so.2")
gl.glFinish.argtypes = []
gl.glReadPixels.argtypes = [
    C.c_int,
    C.c_int,
    C.c_int,
    C.c_int,
    C.c_uint,
    C.c_uint,
    C.c_void_p,
]
context = NativeContext("hevc", b"")
reader = av.CodecContext.create("hevc", "r")
packets = reader.parse(open(args.stream, "rb").read()) + reader.parse(b"")
window = create_window(3840, 2160, visible=False)
renderer = None
elapsed = []
frames = 0
errors = 0
sample = None
started = time.perf_counter()
try:
    for i, packet in enumerate(packets):
        packet.pts = i
        t = time.perf_counter()
        output = context.decode(packet)
        for frame in output:
            if renderer is None:
                renderer = Renderer(frame.width, frame.height, frame.generation)
            assert frame.gpu_surface and frame.is_yuv444
            errors += bool(frame.decode_error_flags)
            assert renderer.upload(frame, 0, frame.height)
            fw, fh = glfw.get_framebuffer_size(window)
            renderer.draw(fw, fh)
            gl.glFinish()
            elapsed.append((time.perf_counter() - t) * 1000)
            frames += 1
            if sample is None:
                # Check orientation and color against the same packet decoded
                # in software; this one-off test deliberately reads GPU pixels.
                reference = av.CodecContext.create("hevc", "r")
                decoded = reference.decode(packet) + reference.decode(None)
                cpu = (
                    decoded[0]
                    .reformat(
                        width=fw, height=fh, format="rgb24", src_colorspace="ITU709"
                    )
                    .to_ndarray()
                )
                pixels = np.empty((fh, fw, 4), dtype=np.uint8)
                gl.glReadPixels(0, 0, fw, fh, 0x1908, 0x1401, pixels.ctypes.data)
                gpu = pixels[::-1, :, :3]
                # Sample away from edges where resampling may differ.
                diff = gpu[16:-16:32, 16:-16:32].astype(float) - cpu[
                    16:-16:32, 16:-16:32
                ].astype(float)
                sample = {
                    "mean_abs_rgb_error": round(float(np.abs(diff).mean()), 3),
                    "p99_abs_rgb_error": round(
                        float(np.percentile(np.abs(diff), 99)), 3
                    ),
                }
            if frames >= args.frames:
                break
        if frames >= args.frames:
            break
    result = {
        "frames": frames,
        "decode_errors": errors,
        "canvas": [renderer.width, renderer.height] if renderer else None,
        "completed_pipeline_fps": round(frames / (sum(elapsed) / 1000), 2)
        if elapsed
        else 0,
        "pipeline_ms_p50": round(float(np.percentile(elapsed, 50)), 3)
        if elapsed
        else None,
        "pipeline_ms_p95": round(float(np.percentile(elapsed, 95)), 3)
        if elapsed
        else None,
        "validation_seconds": round(time.perf_counter() - started, 2),
        "color_check": sample,
    }
    print(json.dumps(result, indent=2))
    if (
        frames < min(args.frames, len(packets))
        or errors
        or sample is None
        or sample["mean_abs_rgb_error"] > 5
    ):
        raise SystemExit("Native validation failed")
finally:
    if renderer:
        renderer.close()
    context.close()
    glfw.destroy_window(window)
    glfw.terminate()
