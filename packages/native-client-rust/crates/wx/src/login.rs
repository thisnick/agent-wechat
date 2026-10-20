use super::*;
use image::ImageReader;
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Write};
pub struct Options {
    pub idempotency_key: Option<String>,
    pub force_qr: bool,
    pub no_qr: bool,
    pub timeout: u64,
    pub detach: bool,
    pub qr_output: Option<PathBuf>,
    pub strategy: String,
    pub qr_refresh: String,
    pub max_refreshes: u8,
    pub json: bool,
}
fn terminal(state: &str) -> bool {
    matches!(
        state,
        "succeeded"
            | "failed"
            | "cancelled"
            | "expired"
            | "rejected"
            | "needs_login"
            | "approval_required"
            | "challenge_required"
    )
}
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
/// Render the issued PNG's pixels, never a QR encoding of the PNG bytes. Only
/// exact integral black/white module grids are reduced for the terminal.
fn render(bytes: &[u8]) -> Result<String> {
    let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.to_luma8();
    let (w, h) = image.dimensions();
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    for (y, row) in image.rows().enumerate() {
        for (x, pixel) in row.enumerate() {
            if pixel[0] < 128 {
                left = left.min(x as u32);
                right = right.max(x as u32);
                top = top.min(y as u32);
                bottom = bottom.max(y as u32);
            }
        }
    }
    if left >= right || top >= bottom {
        bail!("QR PNG contains no readable grid");
    }
    let mut step = 0;
    for y in top..=bottom {
        let mut start = left;
        let mut dark = image.get_pixel(left, y)[0] < 128;
        for x in left + 1..=right + 1 {
            let next = x <= right && image.get_pixel(x, y)[0] < 128;
            if x == right + 1 || next != dark {
                step = gcd(step, x - start);
                start = x;
                dark = next;
            }
        }
    }
    if step == 0 || (right - left + 1) % step != 0 || (bottom - top + 1) % step != 0 {
        bail!("QR PNG is not an integral pixel grid");
    }
    let width = (right - left + 1) / step;
    let height = (bottom - top + 1) / step;
    if width > 177 || height > 177 || width != height {
        bail!("QR PNG cannot fit a lossless terminal grid");
    }
    let size = width + 8;
    let dark = |x: u32, y: u32| -> bool {
        x >= 4
            && y >= 4
            && x < size - 4
            && y < size - 4
            && image.get_pixel(left + (x - 4) * step, top + (y - 4) * step)[0] < 128
    };
    let mut out = String::new();
    for y in (0..size).step_by(2) {
        out.push_str("\x1b[30;107m");
        for x in 0..size {
            out.push(match (dark(x, y), y + 1 < size && dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push_str("\x1b[0m\n");
    }
    Ok(out)
}
async fn present(
    r: &Remote,
    settings: &settings::File,
    id: &str,
    generation: u64,
    options: &Options,
) -> Result<PathBuf> {
    let response = r
        .response(
            reqwest::Method::GET,
            &query(
                r.path(&format!("operations/{}/qr", encode(id)))?,
                &[("generation", Some(generation.to_string()))],
            ),
            None,
            None,
        )
        .await?;
    if !response
        .headers()
        .get("content-type")
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| s.starts_with("image/png"))
    {
        bail!("login QR response is not PNG");
    }
    let bytes = bounded_bytes(response, 4 * 1024 * 1024).await?;
    let path = if let Some(path) = &options.qr_output {
        if generation <= 1 {
            path.clone()
        } else {
            path.with_file_name(format!(
                "{}-{generation}.png",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ))
        }
    } else {
        let directory = settings
            .path
            .parent()
            .context("client config directory missing")?
            .join("qr");
        std::fs::create_dir_all(&directory)?;
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("QR output directory must be owned by this user");
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        directory.join(format!(
            "{:x}-{generation}.png",
            Sha256::digest(id.as_bytes())
        ))
    };
    if path.exists() {
        if private_input(&path, 4 * 1024 * 1024)? != bytes {
            bail!("QR output exists with different bytes");
        }
    } else {
        private_output(&path, &bytes)?;
    }
    if !options.json {
        if options.qr_output.is_none() && std::io::stderr().is_terminal() {
            match render(&bytes) {
                Ok(qr) => {
                    std::io::stderr().write_all(qr.as_bytes())?;
                }
                Err(_) => eprintln!("Open the QR PNG to scan it."),
            }
        }
        eprintln!("QR PNG: {}", path.display());
    }
    Ok(path)
}
pub async fn qr(
    r: &Remote,
    settings: &settings::File,
    id: &str,
    output: Option<PathBuf>,
    json: bool,
) -> Result<Value> {
    let operation = r
        .get(&r.path(&format!("operations/{}", encode(id)))?)
        .await?;
    let generation = operation["result"]["qrGeneration"]
        .as_u64()
        .context("login operation has no QR generation")?;
    let path = present(
        r,
        settings,
        id,
        generation,
        &Options {
            idempotency_key: None,
            force_qr: false,
            no_qr: false,
            timeout: 300,
            detach: false,
            qr_output: output,
            strategy: "auto".into(),
            qr_refresh: "auto".into(),
            max_refreshes: 3,
            json,
        },
    )
    .await?;
    Ok(json!({"operationId":id,"qrGeneration":generation,"path":path}))
}
pub async fn login(r: &Remote, settings: &settings::File, options: Options) -> Result<Value> {
    if !(30..=900).contains(&options.timeout) {
        bail!("login timeout must be30..900seconds");
    }
    if options.force_qr && options.strategy != "auto" && options.strategy != "qr" {
        bail!("--qr conflicts with selected login strategy");
    }
    if options.no_qr && options.strategy == "qr" {
        bail!("--saved-only conflicts with QR strategy");
    }
    let strategy = if options.force_qr {
        "qr"
    } else {
        &options.strategy
    };
    let body = json!({"strategy":strategy,"allowQr":!options.no_qr,"timeoutSeconds":options.timeout,"qrRefresh":{"mode":options.qr_refresh,"maxRefreshes":options.max_refreshes}});
    let key = options
        .idempotency_key
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        bail!("invalid login idempotency key");
    }
    let directory = settings
        .path
        .parent()
        .context("client config directory missing")?
        .join("login-receipts");
    std::fs::create_dir_all(&directory)?;
    let meta = std::fs::symlink_metadata(&directory)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        bail!("login receipt directory must be owned by this user");
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let receipt_path = directory.join(format!(
        "{:x}.json",
        Sha256::digest(format!("{}\0{}\0{key}", r.base, r.account()?).as_bytes())
    ));
    let mut receipt = json!({"schemaVersion":1,"endpoint":r.base.as_str(),"accountId":r.account()?,"idempotencyKey":key,"request":body});
    if receipt_path.exists() {
        let old: Value = serde_json::from_slice(&private_input(&receipt_path, 65536)?)?;
        for name in ["endpoint", "accountId", "idempotencyKey", "request"] {
            if old[name] != receipt[name] {
                bail!("idempotency_conflict: login receipt input changed");
            }
        }
        receipt = old;
    } else {
        private_output(&receipt_path, &serde_json::to_vec(&receipt)?)?;
    }
    let mut operation = if let Some(id) = receipt["operationId"].as_str() {
        r.get(&r.path(&format!("operations/{}", encode(id)))?)
            .await?
    } else {
        r.request(
            reqwest::Method::POST,
            &r.path("auth/login")?,
            Some(body),
            Some(&key),
        )
        .await
        .with_context(|| {
            format!(
                "login response unavailable; retain original attempt receipt {}",
                receipt_path.display()
            )
        })?
    };
    receipt["operationId"] = operation["id"].clone();
    client_actions::write_replace(&receipt_path, &receipt)?;
    operation["receiptPath"] = json!(receipt_path);
    if options.detach {
        return Ok(operation);
    }
    let id = operation["id"]
        .as_str()
        .context("invalid login operation identity")?
        .to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(options.timeout + 35);
    let mut shown = 0;
    loop {
        if terminal(operation["state"].as_str().unwrap_or("")) {
            return Ok(operation);
        }
        if std::time::Instant::now() >= deadline {
            operation["clientWaitExpired"] = json!(true);
            return Ok(operation);
        }
        let generation = operation["result"]["qrGeneration"].as_u64().unwrap_or(0);
        if operation["result"]["qrAvailable"] == true && generation > shown {
            let path = present(r, settings, &id, generation, &options).await?;
            operation["qrPath"] = json!(path);
            shown = generation;
        }
        let next = tokio::select! {
            _=tokio::signal::ctrl_c()=>{let cancellation=tokio::time::timeout(Duration::from_secs(3),r.request(reqwest::Method::DELETE,&r.path(&format!("operations/{}",encode(&id)))?,None,None)).await;match cancellation{Ok(Ok(value))=>{operation=value;operation["cancellationConfirmed"]=json!(true);},_=>{operation["cancellationConfirmed"]=json!(false);}}operation["clientInterrupted"]=json!(true);operation["receiptPath"]=json!(receipt_path);set_exit(130);return Ok(operation);},
            result=async{tokio::time::sleep(Duration::from_millis(500)).await;r.get(&r.path(&format!("operations/{}",encode(&id)))?).await}=>result
        };
        let qr_path = operation.get("qrPath").cloned();
        operation = next?;
        operation["receiptPath"] = json!(receipt_path);
        if let Some(path) = qr_path {
            operation["qrPath"] = path;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn white_terminal_qr_uses_issued_pixel_grid_with_quiet_border() {
        let mut image = image::GrayImage::from_pixel(21 * 3 + 16, 21 * 3 + 16, image::Luma([255]));
        for y in 0..21 {
            for x in 0..21 {
                if (x + y) % 2 == 0 {
                    for dy in 0..3 {
                        for dx in 0..3 {
                            image.put_pixel(8 + x * 3 + dx, 8 + y * 3 + dy, image::Luma([0]));
                        }
                    }
                }
            }
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let qr = render(bytes.get_ref()).unwrap();
        assert!(qr.starts_with("\x1b[30;107m    "));
        assert_eq!(qr.lines().count(), 15);
        assert!(qr.contains('▀'));
    }
}
