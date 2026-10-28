//! Images (PNG, JPEG, WebP): OCR with the pure-Rust `ocrs` engine, only when
//! `ExtractOptions::ocr` is set and both model files exist. Models are never downloaded.

use nugget_core::{Error, Result};

use crate::{Ctx, Extracted, OCR_MODELS_ENV};

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    if !ctx.opts.ocr {
        return Err(Error::Unsupported("image: OCR is disabled".into()));
    }
    let Some(models) = ctx.ocr else {
        return Err(Error::Unsupported(format!(
            "image: no OCR models configured (set {OCR_MODELS_ENV})"
        )));
    };
    if !models.exist() {
        return Err(Error::Unsupported(format!(
            "image: OCR model files not found ({}, {})",
            models.detection.display(),
            models.recognition.display()
        )));
    }
    imp::run(data, models)
}

#[cfg(feature = "ocr")]
mod imp {
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    use nugget_core::{Error, Result};
    use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
    use serde_json::json;

    use crate::{Extracted, OcrModels};

    /// Largest accepted image side, in pixels.
    const MAX_DIM: u32 = 12_000;
    /// Largest decoder allocation.
    const MAX_ALLOC: u64 = 512 * 1024 * 1024;

    /// One loaded engine, reused while the model paths stay the same.
    static ENGINE: Mutex<Option<(OcrModels, Arc<OcrEngine>)>> = Mutex::new(None);

    fn engine(models: &OcrModels) -> Result<Arc<OcrEngine>> {
        let mut guard = ENGINE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((m, e)) = guard.as_ref()
            && m == models
        {
            return Ok(Arc::clone(e));
        }
        let load = |path: &std::path::Path| {
            rten::Model::load_file(path).map_err(|e| {
                Error::Unsupported(format!("OCR model {} is unusable: {e}", path.display()))
            })
        };
        let params = OcrEngineParams {
            detection_model: Some(load(&models.detection)?),
            recognition_model: Some(load(&models.recognition)?),
            ..Default::default()
        };
        let engine = Arc::new(
            OcrEngine::new(params)
                .map_err(|e| Error::Unsupported(format!("OCR engine unusable: {e}")))?,
        );
        *guard = Some((models.clone(), Arc::clone(&engine)));
        Ok(engine)
    }

    fn decode(data: &[u8]) -> Result<image::RgbImage> {
        let mut reader = image::ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| Error::Invalid(format!("image: {e}")))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DIM);
        limits.max_image_height = Some(MAX_DIM);
        limits.max_alloc = Some(MAX_ALLOC);
        reader.limits(limits);
        let img = reader
            .decode()
            .map_err(|e| Error::Invalid(format!("image: cannot decode: {e}")))?;
        Ok(img.into_rgb8())
    }

    pub(super) fn run(data: &[u8], models: &OcrModels) -> Result<Extracted> {
        let img = decode(data)?;
        let (width, height) = img.dimensions();
        let engine = engine(models)?;
        let source = ImageSource::from_bytes(img.as_raw(), (width, height))
            .map_err(|e| Error::Invalid(format!("image: {e}")))?;
        let input = engine
            .prepare_input(source)
            .map_err(|e| Error::Other(format!("OCR failed: {e}")))?;
        let text = engine
            .get_text(&input)
            .map_err(|e| Error::Other(format!("OCR failed: {e}")))?;
        Ok(Extracted {
            text: text.trim().to_owned(),
            metadata: json!({"ocr": true, "engine": "ocrs", "width": width, "height": height}),
            ..Default::default()
        })
    }
}

#[cfg(not(feature = "ocr"))]
mod imp {
    use nugget_core::{Error, Result};

    use crate::{Extracted, OcrModels};

    pub(super) fn run(_data: &[u8], _models: &OcrModels) -> Result<Extracted> {
        Err(Error::Unsupported(
            "image: built without the `ocr` feature".into(),
        ))
    }
}
