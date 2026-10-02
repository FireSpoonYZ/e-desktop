"""Offline only: real fixture renderer -> padded outer BGRA -> actual GDI proxy -> analyzer.
No HWND, source capture, fixture GUI, desktop input, video, or readiness inference.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys


def rectangle(x, y, width, height):
    return dict(x=x, y=y, width=width, height=height)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--analyzer", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    probe, fixture, analyzer, root = (p.resolve() for p in
                                    (args.probe, args.fixture, args.analyzer, args.output))
    root.mkdir()  # Never overwrite prior evidence.
    spec = importlib.util.spec_from_file_location("handoff_analysis", analyzer)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    subprocess.run([str(fixture), "--self-test"], check=True)
    reference = root / "reference.bmp"
    cw, ch, sw, sh, pad_x, pad_y = 700, 467, 716, 506, 8, 31
    subprocess.run([str(fixture), "--tag", "offline-interop", "--role", "C",
                    "--render-reference", str(reference), "--generation", "7",
                    "--width", str(cw), "--height", str(ch)], check=True)
    client = module.load_bmp(reference)
    pixels = bytearray(bytes([20, 30, 40, 255]) * (sw * sh))
    for y in range(ch):
        start = ((y + pad_y) * sw + pad_x) * 4
        pixels[start:start + cw * 4] = client.bgra[y * cw * 4:(y + 1) * cw * 4]
    source = root / "outer.bgra"
    source.write_bytes(pixels)
    results = []
    # Nonclient padding is constructed physical geometry, not a desktop measurement.
    # Chrome fill is irrelevant: the analyzer must compare the entire projected real client.
    for index, (dw, dh) in enumerate([(716, 506), (537, 379), (895, 633), (633, 457)]):
        name = f"case-{index}-{dw}x{dh}"
        out = root / name
        canvas_w, canvas_h, dx, dy = dw + 34, dh + 26, 17, 13
        config = dict(input=str(source), sourceWidth=sw, sourceHeight=sh,
                      canvasWidth=canvas_w, canvasHeight=canvas_h,
                      destination=rectangle(dx, dy, dw, dh), output=str(out))
        config_file = root / (name + ".json")
        config_file.write_text(json.dumps(config), encoding="utf-8")
        subprocess.run([str(probe), "--offline-proxy", str(config_file)], check=True)
        metadata = json.loads((out / "proxy.json").read_text())
        assert metadata["captureSampling"] == "identity"
        assert metadata["proxySampling"] == "GDI-COLORONCOLOR"
        manifest = dict(schemaVersion=1, sampling="nearest",
                        frame=dict(path="proxy.bgra", format="bgra", width=canvas_w, height=canvas_h),
                        reference=dict(path=str(reference), format="bmp"),
                        expected=dict(generation=7, width=cw, height=ch, role="C"),
                        sourceRect=rectangle(-900, 50, sw, sh),
                        clientRect=rectangle(-900+pad_x, 50+pad_y, cw, ch),
                        destinationRect=rectangle(dx, dy, dw, dh),
                        clipRect=rectangle(0, 0, canvas_w, canvas_h))

        def assess(label, value, expected_pass):
            path = out / (label + "-manifest.json")
            path.write_text(json.dumps(value), encoding="utf-8")
            output = out / (label + "-analysis.json")
            run = subprocess.run([sys.executable, str(analyzer), str(path), "--output", str(output)],
                                 capture_output=True, text=True)
            result = json.loads(output.read_text())
            results.append(dict(case=name, check=label, status=result["status"],
                                exitCode=run.returncode, comparedPixels=result.get("comparedPixels"),
                                reason=result.get("reason"), semanticReady=result["semanticReady"]))
            (root / "results.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
            assert (result["status"] == "pass") == expected_pass, results[-1]
            assert run.returncode == (0 if expected_pass else 2), results[-1]

        assess("positive", manifest, True)
        original = (out / "proxy.bgra").read_bytes()
        # Corrupt an actual projected TL border and a separate customer interior sample.
        for label, sx, sy in [("damaged-corner", pad_x+8, pad_y+8),
                               ("damaged-interior", pad_x+cw//2, pad_y+ch//2)]:
            bad = bytearray(original)
            x, y = int(dx+(sx+0.5)*dw/sw), int(dy+(sy+0.5)*dh/sh)
            for yy in range(y, min(y+3, canvas_h)):
                for xx in range(x, min(x+3, canvas_w)):
                    offset = (yy*canvas_w+xx)*4
                    bad[offset:offset+4] = bytes([123, 45, 67, 255])
            filename = label + ".bgra"
            (out / filename).write_bytes(bad)
            wrong = dict(manifest, frame=dict(manifest["frame"], path=filename))
            assess(label, wrong, False)
        wrong = dict(manifest, clientRect=dict(manifest["clientRect"],
                                              x=manifest["clientRect"]["x"]+2))
        assess("wrong-client-padding", wrong, False)
    print(json.dumps(results, indent=2))
    print("PASS: offline single-proxy producer pixels only; synthetic nonclient geometry; no visual/DWM acceptance.")


if __name__ == "__main__":
    main()
