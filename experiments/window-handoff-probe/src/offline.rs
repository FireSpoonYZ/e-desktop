//! File/memory only. The same Dib::render used by the frozen window proxy does the transform.
use super::*;
use std::io::Read;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Spec {
    input: std::path::PathBuf,
    source_width: i32,
    source_height: i32,
    canvas_width: i32,
    canvas_height: i32,
    destination: Rect,
    output: std::path::PathBuf,
}

pub fn offline_proxy(path: &Path) -> Result<(), String> {
    let spec: Spec = serde_json::from_reader(File::open(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let bytes = pixel_bytes(spec.source_width, spec.source_height, FRAME_BYTES)?;
    pixel_bytes(spec.canvas_width, spec.canvas_height, SCENE_BYTES)?;
    let destination = Rect::new(
        spec.destination.x,
        spec.destination.y,
        spec.destination.width,
        spec.destination.height,
    )?;
    if !Rect::new(0, 0, spec.canvas_width, spec.canvas_height)?.contains(destination) {
        return Err("offline destination must fit the canvas".into());
    }
    let input = File::open(&spec.input).map_err(|e| e.to_string())?;
    let info = input.metadata().map_err(|e| e.to_string())?;
    if !info.is_file() || info.len() != bytes as u64 {
        return Err(
            "offline input must be a regular tightly packed top-down BGRA file of exact size"
                .into(),
        );
    }
    let mut pixels = Vec::with_capacity(bytes);
    input
        .take(bytes as u64 + 1)
        .read_to_end(&mut pixels)
        .map_err(|e| e.to_string())?;
    if pixels.len() != bytes {
        return Err("offline input size changed".into());
    }
    let mut source = Dib::new(spec.source_width, spec.source_height, FRAME_BYTES)?;
    source.pixels().copy_from_slice(&pixels);
    let mut scene = Dib::new(spec.canvas_width, spec.canvas_height, SCENE_BYTES)?;
    scene.render(&source, destination)?;
    fs::create_dir(&spec.output).map_err(|e| format!("new offline output directory: {e}"))?;
    fs::write(spec.output.join("proxy.bgra"), scene.pixels()).map_err(|e| e.to_string())?;
    let metadata = json!({"schemaVersion": 1, "format": "BGRA8-top-down", "sourceWidth": spec.source_width,
        "sourceHeight": spec.source_height, "width": spec.canvas_width, "height": spec.canvas_height,
        "destinationRect": destination, "captureSampling": "identity", "proxySampling": "GDI-COLORONCOLOR",
        "transformCount": 1, "semanticReady": null, "visualPass": null, "memoryDcOnly": true});
    fs::write(
        spec.output.join("proxy.json"),
        serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "Offline identity capture + single COLORONCOLOR proxy written; no HWND/capture; not visual acceptance."
    );
    Ok(())
}
