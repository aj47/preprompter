//! S3 upload client for screen captures.

use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use chrono::{DateTime, Utc};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::capture::CapturedFrame;
use crate::config::S3Config;

/// Result of an S3 upload operation.
#[derive(Debug, Clone)]
pub struct UploadResult {
    /// S3 key where the object was uploaded.
    pub key: String,
    /// ETag of the uploaded object.
    pub etag: String,
    /// Upload timestamp.
    pub uploaded_at: DateTime<Utc>,
    /// Duration of the upload operation.
    pub upload_duration_ms: u64,
}

/// S3 uploader client.
pub struct S3Uploader {
    client: Client,
    bucket: String,
    prefix: Option<String>,
    retry_attempts: u32,
}

impl S3Uploader {
    /// Create a new S3 uploader with the given configuration.
    pub async fn new(config: &S3Config) -> Result<Self> {
        let mut aws_config_builder = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()));

        // Apply custom endpoint if specified (for R2, MinIO, etc.)
        if let Some(endpoint) = &config.endpoint_url {
            if !endpoint.is_empty() {
                aws_config_builder = aws_config_builder.endpoint_url(endpoint);
            }
        }

        let aws_config = aws_config_builder.load().await;
        let client = Client::new(&aws_config);

        info!(
            "S3 uploader initialized: bucket={}, region={}",
            config.bucket, config.region
        );

        Ok(Self {
            client,
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
            retry_attempts: 3,
        })
    }

    /// Set the number of retry attempts for failed uploads.
    pub fn with_retry_attempts(mut self, attempts: u32) -> Self {
        self.retry_attempts = attempts;
        self
    }

    /// Upload a captured frame to S3.
    pub async fn upload_frame(&self, frame: &CapturedFrame) -> Result<UploadResult> {
        let key = frame.s3_key(self.prefix.as_deref());
        let data = frame.data.clone();

        self.upload_bytes(&key, data, "image/jpeg").await
    }

    /// Upload raw bytes to S3 with retries.
    pub async fn upload_bytes(
        &self,
        key: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<UploadResult> {
        let start = Instant::now();
        let mut last_error = None;

        for attempt in 0..self.retry_attempts {
            if attempt > 0 {
                // Exponential backoff
                let delay = Duration::from_millis(100 * 2u64.pow(attempt));
                debug!("Retry attempt {} after {:?}", attempt + 1, delay);
                tokio::time::sleep(delay).await;
            }

            match self.do_upload(key, data.clone(), content_type).await {
                Ok(etag) => {
                    let duration = start.elapsed();
                    return Ok(UploadResult {
                        key: key.to_string(),
                        etag,
                        uploaded_at: Utc::now(),
                        upload_duration_ms: duration.as_millis() as u64,
                    });
                }
                Err(e) => {
                    warn!("Upload attempt {} failed: {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Upload failed with no error")))
    }

    /// Perform the actual S3 upload.
    async fn do_upload(&self, key: &str, data: Vec<u8>, content_type: &str) -> Result<String> {
        let body = ByteStream::from(data);

        let response = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(body)
            .send()
            .await
            .with_context(|| format!("Failed to upload to s3://{}/{}", self.bucket, key))?;

        let etag = response
            .e_tag()
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();

        debug!("Uploaded {} -> s3://{}/{}", etag, self.bucket, key);

        Ok(etag)
    }

    /// Upload multiple frames in batch.
    pub async fn upload_batch(&self, frames: Vec<CapturedFrame>) -> Result<Vec<UploadResult>> {
        let mut results = Vec::with_capacity(frames.len());
        let mut errors = Vec::new();

        for frame in frames {
            match self.upload_frame(&frame).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    error!("Failed to upload frame {}: {}", frame.frame_id(), e);
                    errors.push(e);
                }
            }
        }

        if !errors.is_empty() {
            warn!("{} frames failed to upload", errors.len());
        }

        Ok(results)
    }
}

