"""Offline fixture-client evidence, NOT third-party semantic readiness.

Manifest schemaVersion=1:
  frame: {path, format: bmp|bgra|video, width?, height?, frameIndex?}
  expected: {generation: uint32, width, height, role: A..E}
  sourceRect, clientRect, destinationRect, clipRect: {x,y,width,height}
  sampling: "nearest"
  reference: {path, format: bmp|bgra, width?, height?}
  fixtureState?: the atomic fixture state snapshot (reported separately).

sourceRect/clientRect are physical screen coordinates at capture. destinationRect
maps sourceRect into decoded frame coordinates, not desktop coordinates. clipRect
is in that decoded frame. x'=dst.x+(x-src.x)*dst.width/src.width (likewise y).
Each destination pixel center maps back via floor to a reference client pixel.
This contract deliberately supports only axis-aligned nearest-neighbor transforms.
Missing/partial/ambiguous observations are unknown, never pass.

Markers: 12x10 cells, 4px/cell, inset 8 client pixels; TL/TR/BL/BR borders
RGB (0,208,208)/(208,0,208)/(208,208,0)/(240,112,0).
Inner 10x8 cells, row-major LSB-first: LE uint32 generation, LE uint16 width,
LE uint16 height, ASCII role, XOR(previous 9 bytes, seed 0xA7). 0=RGB16, 1=RGB240.
Marker-only agreement is insufficient: pass additionally requires exact comparison
of EVERY projected client pixel to an offline reference produced by the fixture's
--render-reference command. Lossy video will usually be unknown. No tolerance
turns compressed/damaged pixels into pass. A pass covers fixture CLIENT pixels,
not title bar, input, compositor delivery, or readiness of any third-party app.
"""
import argparse
from dataclasses import dataclass
import json
import math
from pathlib import Path
import struct
import subprocess

CELL, INSET, MW, MH = 4, 8, 48, 40
BORDERS = ((0, 208, 208), (208, 0, 208), (208, 208, 0), (240, 112, 0))
ZERO, ONE = (16, 16, 16), (240, 240, 240)


@dataclass(frozen=True)
class Pixels:
    width: int
    height: int
    bgra: bytes

    def __post_init__(self):
        if not (0 < self.width <= 4096 and 0 < self.height <= 4096):
            raise ValueError("unsupported pixel dimensions")
        if len(self.bgra) != self.width * self.height * 4:
            raise ValueError("truncated or extra BGRA bytes")

    def rgb(self, x, y):
        if not (0 <= x < self.width and 0 <= y < self.height):
            raise ValueError("sample outside frame")
        i = (y * self.width + x) * 4
        b, g, r = self.bgra[i:i+3]
        return r, g, b


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError("invalid integer")
    return value


def load_bmp(path):
    data = Path(path).read_bytes()
    if len(data) < 54 or data[:2] != b"BM":
        raise ValueError("not a BMP")
    offset = struct.unpack_from("<I", data, 10)[0]
    dib = struct.unpack_from("<I", data, 14)[0]
    if dib < 40 or 14 + dib > len(data):
        raise ValueError("unsupported BMP header")
    width, signed_height, planes, bits, compression = struct.unpack_from("<iiHHI", data, 18)
    height = abs(signed_height)
    if not (0 < width <= 4096 and 0 < height <= 4096 and planes == 1 and bits in (24, 32) and compression == 0):
        raise ValueError("BMP must be bounded uncompressed 24/32-bit RGB")
    stride = ((width * bits + 31) // 32) * 4
    if offset < 14 + dib or offset + stride * height > len(data):
        raise ValueError("truncated BMP")
    result = bytearray(width * height * 4)
    for y in range(height):
        row = offset + (y if signed_height < 0 else height-1-y) * stride
        for x in range(width):
            i, out = row+x*(bits//8), (y*width+x)*4
            result[out:out+3] = data[i:i+3]
            result[out+3] = 255
    return Pixels(width, height, bytes(result))


def load_pixels(spec, base=Path("."), allow_video=True):
    # Resolve only a regular offline file. No FFmpeg input devices, URLs, or capture.
    path = (base / spec["path"]).resolve()
    if not path.is_file():
        raise ValueError("offline input must be an existing regular file")
    kind = spec["format"]
    if kind == "bmp":
        return load_bmp(path)
    width = integer(spec["width"], 1, 4096)
    height = integer(spec["height"], 1, 4096)
    if kind == "bgra":
        return Pixels(width, height, path.read_bytes())
    if kind != "video" or not allow_video:
        raise ValueError("unsupported offline format")
    index = integer(spec.get("frameIndex", 0), 0, 10000000)
    # Restrict protocols even for malicious local playlists; no screen/camera input.
    command = ["ffmpeg", "-v", "error", "-nostdin", "-protocol_whitelist", "file",
               "-i", str(path), "-vf", "select=eq(n\\," + str(index) + ")",
               "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"]
    decoded = subprocess.run(command, check=True, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, timeout=60).stdout
    return Pixels(width, height, decoded)


def rect(value):
    values = tuple(value[key] for key in ("x", "y", "width", "height"))
    if any(type(v) not in (int, float) or not math.isfinite(v) for v in values):
        raise ValueError("non-finite rectangle")
    if values[2] <= 0 or values[3] <= 0:
        raise ValueError("empty rectangle")
    return values


def contains(outer, inner):
    x, y, w, h = outer
    a, b, c, d = inner
    return x <= a and y <= b and a+c <= x+w and b+d <= y+h


def expected_layout(value):
    generation = integer(value["generation"], 1, 0xFFFFFFFF)
    width = integer(value["width"], 128, 4096)
    height = integer(value["height"], 112, 4096)
    role = value["role"]
    if role not in ("A", "B", "C", "D", "E"):
        raise ValueError("invalid role")
    return dict(generation=generation, width=width, height=height, role=role)


def marker_origin(corner, width, height):
    return (INSET if corner % 2 == 0 else width-INSET-MW,
            INSET if corner < 2 else height-INSET-MH)


def decode_marker(pixels, corner, width, height, project):
    ox, oy = marker_origin(corner, width, height)
    payload = bytearray(10)
    for row in range(10):
        for column in range(12):
            # No fuzzy matching: a damaged or ambiguous sample is unknown.
            x, y = project(ox+(column+0.5)*CELL, oy+(row+0.5)*CELL)
            color = pixels.rgb(math.floor(x), math.floor(y))
            border = row in (0, 9) or column in (0, 11)
            if border:
                if color != BORDERS[corner]:
                    raise ValueError("marker border missing/occluded/damaged")
            else:
                if color not in (ZERO, ONE):
                    raise ValueError("marker bit ambiguous/damaged")
                bit = (row-1)*10+column-1
                if color == ONE:
                    payload[bit//8] |= 1 << (bit % 8)
    checksum = 0xA7
    for byte in payload[:9]:
        checksum ^= byte
    if checksum != payload[9]:
        raise ValueError("marker checksum mismatch")
    generation, pw, ph = struct.unpack_from("<IHH", payload)
    role = chr(payload[8])
    if generation == 0 or role not in "ABCDE" or not (128 <= pw <= 4096 and 112 <= ph <= 4096):
        raise ValueError("invalid marker payload")
    return dict(generation=generation, width=pw, height=ph, role=role)


def assess_fixture_state(state):
    """A bitmap publication observation only; never compositor/semantic ready."""
    try:
        if type(state["schemaVersion"]) is not int or state["schemaVersion"] != 1 or state["clock"] != "QueryPerformanceCounter" or state["clockUnit"] != "ticks":
            raise ValueError("unsupported state schema/clock")
        integer(state["qpcFrequency"], 1, 10**12)
        integer(state["qpcTicks"], 0, 2**63-1)
        desired = integer(state["desiredGeneration"], 1, 0xFFFFFFFF)
        painted = integer(state["paintedGeneration"], 1, 0xFFFFFFFF)
        if painted > desired or type(state["pending"]) is not bool:
            raise ValueError("inconsistent generations")
        d, p = state["desiredClientSize"], state["paintedClientSize"]
        for size in (d, p):
            integer(size["width"], 1, 4096)
            integer(size["height"], 1, 4096)
        pending = painted != desired or d != p
        if pending != state["pending"]:
            raise ValueError("inconsistent pending state")
        return {"status": "pending" if pending else "bitmap-published",
                "desiredGeneration": desired, "paintedGeneration": painted,
                "semanticReady": None}
    except (KeyError, TypeError, ValueError) as error:
        return {"status": "unknown", "reason": str(error), "semanticReady": None}


def analyze(manifest, frame, reference=None):
    result = {"schemaVersion": 1, "status": "unknown", "coverage": "fixture-client-only",
              "semanticReady": None, "markers": []}
    try:
        if "fixtureState" in manifest:
            result["stateObservation"] = assess_fixture_state(manifest["fixtureState"])
        if type(manifest["schemaVersion"]) is not int or manifest["schemaVersion"] != 1 or manifest["sampling"] != "nearest":
            raise ValueError("unsupported schema or sampling (only recorded nearest supported)")
        expected = expected_layout(manifest["expected"])
        sx, sy, sw, sh = rect(manifest["sourceRect"])
        cx, cy, cw, ch = rect(manifest["clientRect"])
        dx, dy, dw, dh = rect(manifest["destinationRect"])
        clip = rect(manifest["clipRect"])
        if not contains((sx, sy, sw, sh), (cx, cy, cw, ch)):
            raise ValueError("client not contained in source geometry")
        if (cw, ch) != (expected["width"], expected["height"]):
            raise ValueError("recorded client size differs from expected layout")
        scale_x, scale_y = dw/sw, dh/sh
        if scale_x < 0.5 or scale_y < 0.5:
            raise ValueError("markers undersampled (minimum scale 0.5)")
        def project(x, y):
            return dx+(cx-sx+x)*scale_x, dy+(cy-sy+y)*scale_y
        left, top = project(0, 0)
        projected = left, top, cw*scale_x, ch*scale_y
        if not contains((0, 0, frame.width, frame.height), projected) or not contains(clip, projected):
            raise ValueError("client clipped or outside decoded frame")
        for corner in range(4):
            result["markers"].append(decode_marker(frame, corner, expected["width"], expected["height"], project))
        if any(marker != expected for marker in result["markers"]):
            result.update(status="mismatch", reason="recognized markers differ from expected layout or each other")
            return result
        result["markerConsistent"] = True
        if reference is None:
            raise ValueError("whole-client reference missing; marker agreement alone is not pass")
        if (reference.width, reference.height) != (cw, ch):
            raise ValueError("reference dimensions differ")
        for corner in range(4):
            if decode_marker(reference, corner, reference.width, reference.height, lambda x, y: (x, y)) != expected:
                raise ValueError("reference marker disagrees with expected layout")
        # Compare ALL destination pixel centers inside the projected client, including text/interior.
        compared = 0
        for y in range(math.ceil(top-0.5), math.ceil(top+projected[3]-0.5)):
            ry = math.floor((y+0.5-top)/scale_y)
            for x in range(math.ceil(left-0.5), math.ceil(left+projected[2]-0.5)):
                rx = math.floor((x+0.5-left)/scale_x)
                if frame.rgb(x, y) != reference.rgb(rx, ry):
                    raise ValueError("client pixel mismatch: mixed/stale/occluded/compressed or incorrect transform")
                compared += 1
        if compared == 0:
            raise ValueError("empty comparison")
        result.update(status="pass", reason="four markers and every projected client pixel match reference",
                      comparedPixels=compared)
    except (KeyError, TypeError, ValueError, OverflowError) as error:
        result["reason"] = str(error)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        manifest = json.loads(args.manifest.read_text(encoding="utf-8-sig"))
        base = args.manifest.resolve().parent
        frame = load_pixels(manifest["frame"], base)
        reference = load_pixels(manifest["reference"], base, allow_video=False) if "reference" in manifest else None
        result = analyze(manifest, frame, reference)
    except (OSError, KeyError, TypeError, ValueError, struct.error, subprocess.SubprocessError) as error:
        result = {"schemaVersion": 1, "status": "unknown", "reason": str(error),
                  "coverage": "fixture-client-only", "semanticReady": None}
    encoded = json.dumps(result, ensure_ascii=False, indent=2)
    if args.output:
        args.output.write_text(encoded+"\n", encoding="utf-8")
    print(encoded)
    # Exit codes distinguish pixel evidence, but are not a visual/product acceptance claim.
    return {"pass": 0, "mismatch": 1, "unknown": 2}[result["status"]]


if __name__ == "__main__":
    raise SystemExit(main())
