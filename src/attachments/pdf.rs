use lopdf::Document;

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("invalid PDF format")]
    InvalidPdfFormat,
    #[error("password required to open encrypted PDF")]
    PasswordRequired,
    #[error("incorrect password provided")]
    WrongPassword,
    #[error("unsupported PDF encryption or structure: {0}")]
    UnsupportedEncryption(String),
    #[error("document extraction failed: {0}")]
    ExtractionFailed(String),
    #[error("document contains no sufficiently certain timeline facts")]
    NoFactsFound,
}

pub fn parse_pdf(bytes: &[u8], password: Option<&str>) -> Result<String, PdfError> {
    if bytes.len() < 32 || bytes.len() > 50_000_000 {
        return Err(PdfError::InvalidPdfFormat);
    }

    if !bytes.starts_with(b"%PDF-")
        && !bytes[..1024.min(bytes.len())]
            .windows(5)
            .any(|w| w == b"%PDF-")
    {
        return Err(PdfError::InvalidPdfFormat);
    }

    let mut doc = Document::load_mem(bytes).map_err(|_e| PdfError::InvalidPdfFormat)?;

    if doc.is_encrypted() {
        let pwd = password.unwrap_or("");
        if pwd.is_empty() {
            return Err(PdfError::PasswordRequired);
        }

        match doc.decrypt(pwd) {
            Ok(()) => {}
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("password") || err_str.contains("authenticated") {
                    return Err(PdfError::WrongPassword);
                } else if err_str.contains("unsupported") || err_str.contains("algorithm") {
                    return Err(PdfError::UnsupportedEncryption(e.to_string()));
                } else {
                    return Err(PdfError::ExtractionFailed(e.to_string()));
                }
            }
        }
    }

    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    if pages.len() > 500 {
        return Err(PdfError::ExtractionFailed(
            "PDF exceeds the 500-page extraction limit".into(),
        ));
    }
    if pages.is_empty() {
        return Err(PdfError::ExtractionFailed("No pages found in PDF".into()));
    }

    let text = doc
        .extract_text_with_limit(&pages, 50_000_000)
        .map_err(|e| PdfError::ExtractionFailed(e.to_string()))?;

    if text.trim().is_empty() {
        return Err(PdfError::ExtractionFailed(
            "PDF contains no extractable text (raster or image only)".into(),
        ));
    }

    Ok(text)
}
