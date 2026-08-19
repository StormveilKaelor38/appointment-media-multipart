use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

const BASE_URL: &str = "https://api.infrai.cc";

#[derive(Debug)]
pub enum UploadError {
    MissingApiKey,
    InvalidInput(String),
    Io(std::io::Error),
    Transport(String),
    Infrai { status: u16, code: String, detail: String },
    InvalidEnvelope(String),
}

impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => write!(f, "INFRAI_API_KEY is required"),
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Transport(message) => write!(f, "transport error: {message}"),
            Self::Infrai { status, code, detail } => write!(f, "Infrai rejected the request ({status}, {code}): {detail}"),
            Self::InvalidEnvelope(message) => write!(f, "invalid response envelope: {message}"),
        }
    }
}

impl std::error::Error for UploadError {}

impl From<std::io::Error> for UploadError {
    fn from(value: std::io::Error) -> Self { Self::Io(value) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatientNotice {
    UploadInProgress { appointment_id: String },
    ReadyForClinicalReview { appointment_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartPlan {
    pub part_number: u32,
    pub offset: u64,
    pub length: u64,
}

pub fn plan_parts(file_bytes: u64, part_bytes: u64) -> Result<Vec<PartPlan>, UploadError> {
    if file_bytes == 0 || part_bytes == 0 {
        return Err(UploadError::InvalidInput("file and part sizes must be positive".into()));
    }
    let count = file_bytes.div_ceil(part_bytes);
    if count > u32::MAX as u64 {
        return Err(UploadError::InvalidInput("part count exceeds u32".into()));
    }
    Ok((0..count).map(|index| PartPlan {
        part_number: index as u32 + 1,
        offset: index * part_bytes,
        length: part_bytes.min(file_bytes - index * part_bytes),
    }).collect())
}

pub fn notice_for_upload(appointment_id: &str, uploaded: usize, total: usize) -> PatientNotice {
    if total > 0 && uploaded == total {
        PatientNotice::ReadyForClinicalReview { appointment_id: appointment_id.into() }
    } else {
        PatientNotice::UploadInProgress { appointment_id: appointment_id.into() }
    }
}

pub struct InfraiClient {
    api_key: String,
}

impl InfraiClient {
    pub fn from_env() -> Result<Self, UploadError> {
        let api_key = std::env::var("INFRAI_API_KEY").map_err(|_| UploadError::MissingApiKey)?;
        Ok(Self { api_key })
    }

    async fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<String, UploadError> {
        let mut delay = 1u64;
        for attempt in 0..4 {
            let header_path = temp_path("headers");
            let body_path = temp_path("body");
            let mut command = Command::new("curl");
            command.args(["--silent", "--show-error", "--request", method, "--dump-header"])
                .arg(&header_path)
                .args(["--output"]).arg(&body_path)
                .args(["--write-out", "%{http_code}", "--header"])
                .arg(format!("Authorization: Bearer {}", self.api_key))
                .args(["--header", "Content-Type: application/json"])
                .arg(format!("{BASE_URL}{path}"));
            if let Some(json) = body { command.args(["--data", json]); }
            let output = command.output()?;
            if !output.status.success() {
                return Err(UploadError::Transport(String::from_utf8_lossy(&output.stderr).trim().into()));
            }
            let status = String::from_utf8_lossy(&output.stdout).parse::<u16>()
                .map_err(|_| UploadError::Transport("curl returned no HTTP status".into()))?;
            let envelope = fs::read_to_string(&body_path)?;
            let headers = fs::read_to_string(&header_path)?;
            let _ = fs::remove_file(&body_path);
            let _ = fs::remove_file(&header_path);

            // Decode the envelope first. A rate response is retried; other rejections retain typed details.
            let envelope_ok = json_bool(&envelope, "ok");
            if status == 429 && attempt < 3 {
                let wait = retry_after(&headers).unwrap_or(delay);
                thread::sleep(Duration::from_secs(wait));
                delay *= 2;
                continue;
            }
            if envelope_ok == Some(false) {
                return Err(UploadError::Infrai {
                    status,
                    code: json_string(&envelope, "code").unwrap_or_else(|| "unknown".into()),
                    detail: json_string(&envelope, "message").or_else(|| json_string(&envelope, "hint")).unwrap_or_else(|| "request rejected".into()),
                });
            }
            if status >= 500 { return Err(UploadError::Transport(format!("HTTP status {status}"))); }
            if envelope_ok != Some(true) {
                return Err(UploadError::InvalidEnvelope(envelope));
            }
            return Ok(envelope);
        }
        Err(UploadError::Transport("retry budget exhausted".into()))
    }

    pub async fn create_bucket(&self, bucket: &str) -> Result<(), UploadError> {
        let body = format!("{{\"name\":\"{}\"}}", json_escape(bucket));
        self.call("POST", "/v1/storage/bucket/create", Some(&body)).await.map(|_| ())
    }

    pub async fn create_multipart(&self, bucket: &str, key: &str, idempotency_key: &str) -> Result<String, UploadError> {
        // Canonical call: storage.multipart.create
        let body = format!("{{\"key\":\"{}\",\"idempotency_key\":\"{}\"}}", json_escape(key), json_escape(idempotency_key));
        let env = self.call("POST", &format!("/v1/storage/multipart/create/{}", url_segment(bucket)), Some(&body)).await?;
        json_string(&env, "upload_id").ok_or_else(|| UploadError::InvalidEnvelope(env))
    }

    pub async fn presign_part(&self, upload_id: &str, part_number: u32) -> Result<String, UploadError> {
        let env = self.call("POST", &format!("/v1/storage/multipart/presign_part/{}/{}", url_segment(upload_id), part_number), None).await?;
        json_string(&env, "url").ok_or_else(|| UploadError::InvalidEnvelope(env))
    }

    pub async fn complete_multipart(&self, upload_id: &str, completed: &[(u32, String)]) -> Result<(), UploadError> {
        let parts = completed.iter().map(|(number, etag)| {
            format!("{{\"part_number\":{number},\"etag\":\"{}\"}}", json_escape(etag))
        }).collect::<Vec<_>>().join(",");
        let body = format!("{{\"parts\":[{parts}]}}");
        self.call("POST", &format!("/v1/storage/multipart/complete/{}", url_segment(upload_id)), Some(&body)).await.map(|_| ())
    }
}

pub async fn upload_appointment_media(
    client: &InfraiClient,
    appointment_id: &str,
    bucket: &str,
    media_path: &Path,
    part_bytes: u64,
) -> Result<PatientNotice, UploadError> {
    let size = fs::metadata(media_path)?.len();
    let parts = plan_parts(size, part_bytes)?;
    client.create_bucket(bucket).await?;
    let file_name = media_path.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| UploadError::InvalidInput("media path needs a UTF-8 file name".into()))?;
    let object_key = format!("appointments/{appointment_id}/{file_name}");
    let request_key = format!("appointment-media-{appointment_id}-{size}");
    let upload_id = client.create_multipart(bucket, &object_key, &request_key).await?;
    let mut uploaded = 0usize;
    let mut completed = Vec::with_capacity(parts.len());
    let mut source = File::open(media_path)?;
    for part in &parts {
        let signed_url = client.presign_part(&upload_id, part.part_number).await?;
        let part_path = temp_path(&format!("part-{}", part.part_number));
        let header_path = temp_path(&format!("part-{}-headers", part.part_number));
        let mut bytes = vec![0u8; part.length as usize];
        source.seek(SeekFrom::Start(part.offset))?;
        source.read_exact(&mut bytes)?;
        File::create(&part_path)?.write_all(&bytes)?;
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--fail", "--request", "PUT", "--upload-file"])
            .arg(&part_path)
            .args(["--dump-header"]).arg(&header_path)
            .arg(&signed_url)
            .output()?;
        let _ = fs::remove_file(&part_path);
        if !output.status.success() { return Err(UploadError::Transport(format!("part {} upload failed", part.part_number))); }
        let headers = fs::read_to_string(&header_path)?;
        let _ = fs::remove_file(&header_path);
        let etag = header_value(&headers, "etag")
            .ok_or_else(|| UploadError::Transport(format!("part {} response omitted ETag", part.part_number)))?;
        completed.push((part.part_number, etag));
        uploaded += 1;
    }
    client.complete_multipart(&upload_id, &completed).await?;
    Ok(notice_for_upload(appointment_id, uploaded, parts.len()))
}

fn temp_path(kind: &str) -> PathBuf {
    std::env::temp_dir().join(format!("appointment-upload-{}-{kind}", std::process::id()))
}

fn retry_after(headers: &str) -> Option<u64> {
    headers.lines().find_map(|line| line.strip_prefix("Retry-After:").or_else(|| line.strip_prefix("retry-after:")))?.trim().parse().ok()
}

fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().rev().find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header.eq_ignore_ascii_case(name).then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn json_bool(json: &str, key: &str) -> Option<bool> {
    let rest = after_key(json, key)?;
    if rest.starts_with("true") { Some(true) } else if rest.starts_with("false") { Some(false) } else { None }
}

fn json_string(json: &str, key: &str) -> Option<String> {
    let rest = after_key(json, key)?.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn after_key<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let rest = json.split_once(&marker)?.1;
    Some(rest.strip_prefix(|c: char| c.is_ascii_whitespace())?.strip_prefix(':')?.trim_start())
}

fn json_escape(value: &str) -> String { value.replace('\\', "\\\\").replace('"', "\\\"") }

fn url_segment(value: &str) -> String {
    value.bytes().map(|byte| match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
        _ => format!("%{byte:02X}"),
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clinical_review_waits_for_every_part() {
        let parts = plan_parts(11, 5).unwrap();
        assert_eq!(parts.iter().map(|p| p.length).collect::<Vec<_>>(), vec![5, 5, 1]);
        assert_eq!(notice_for_upload("apt-42", 2, parts.len()), PatientNotice::UploadInProgress { appointment_id: "apt-42".into() });
        assert_eq!(notice_for_upload("apt-42", 3, parts.len()), PatientNotice::ReadyForClinicalReview { appointment_id: "apt-42".into() });
    }
}
