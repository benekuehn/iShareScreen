"""Synthetic plaintext builders/answer fixture; never records a real session."""
import json
import os
import plistlib
import zlib
from pathlib import Path
from isharescreen.proxy.protocol.rfb import build_virtual_display, build_viewer_info, build_set_encodings
from isharescreen.proxy.protocol.apple import APPLE_VIEWER_COMMAND_MASK, APPLE_0X12_FOLLOWUP
from isharescreen.proxy.protocol.offers import _build_mediablob, _field_varint, _field_bytes
os.environ['ISS_VIDEO_CODEC'] = 'hevc'
os.environ['ISS_TILES_PER_FRAME'] = '1'
os.environ['ISS_LTRP'] = '0'
blob = _field_bytes(5, _field_varint(4, 3840) + _field_varint(5, 2160) + _field_varint(6, 1))
answer = b'\0'*13 + plistlib.dumps({'avcMediaStreamNegotiatorMediaBlob': zlib.compress(blob)}, fmt=plistlib.FMT_BINARY) + b'\x55'*46
Path(__file__).with_name('wire-vector.json').write_text(json.dumps({
  'synthetic_only': True,
  'virtual_display': build_virtual_display(width=1920, height=1080, hidpi_scale=2).hex(),
  'viewer_info': (build_viewer_info(command_mask=APPLE_VIEWER_COMMAND_MASK) + APPLE_0X12_FOLLOWUP).hex(),
  'encodings': build_set_encodings().hex(),
  'video_blob': _build_mediablob(7, 0x12345678, 1234567890, audio_enabled=False).hex(),
  'audio_blob': _build_mediablob(8, 0x12345678, 1234567890, audio_enabled=False).hex(),
  'answer': answer.hex(),
}, indent=2) + '\n')
print('Generated synthetic RFB/media fixture')
