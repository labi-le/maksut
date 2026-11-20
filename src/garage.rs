use crate::*;

/// Object uploads to the S3-compatible store. Keys are content addresses, so a
/// retried upload of the same bytes overwrites an identical object.
pub(crate) struct GarageClient {
    client: aws_sdk_s3::Client,
    bucket: String,
    max_bytes: u64,
    timeout: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GarageError {
    Put,
    Get,
    TooLarge,
}

impl GarageClient {
    /// Builds the client from `[garage]` plus the two credential variables.
    /// Missing credentials or an empty endpoint leave the store disabled rather
    /// than half-configured: ingest then never starts.
    pub(crate) fn from_config() -> Option<Self> {
        let settings = &config().garage;
        if settings.endpoint.trim().is_empty() {
            return None;
        }
        let access = non_empty_env("GARAGE_ACCESS_KEY_ID")?;
        let secret = non_empty_env("GARAGE_SECRET_ACCESS_KEY")?;
        Some(Self::with_settings(settings, &access, &secret))
    }

    pub(crate) fn with_settings(settings: &GarageSettings, access: &str, secret: &str) -> Self {
        let credentials =
            aws_sdk_s3::config::Credentials::new(access, secret, None, None, "maksut-env");
        let shared = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(settings.region.clone()))
            .endpoint_url(settings.endpoint.trim())
            .credentials_provider(credentials)
            // Garage serves buckets as path segments, not host names.
            .force_path_style(true)
            // Only the mandatory checksum is computed: every extra algorithm
            // moves the body into a chunked encoding this client does not need.
            .request_checksum_calculation(
                aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired,
            )
            .response_checksum_validation(
                aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
            )
            .build();
        Self {
            client: aws_sdk_s3::Client::from_conf(shared),
            bucket: settings.bucket.clone(),
            max_bytes: settings.max_bytes,
            timeout: Duration::from_secs(settings.timeout_secs),
        }
    }

    pub(crate) async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), GarageError> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(GarageError::TooLarge);
        }
        let request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes));
        // The SDK retries on its own; this outer bound keeps a stalled socket
        // from holding an ingest permit forever.
        match time::timeout(self.timeout, request.send()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(GarageError::Put),
        }
    }

    /// Fetches one content-addressed object under `max_bytes` and `timeout`.
    pub(crate) async fn get(&self, key: &str) -> Result<Vec<u8>, GarageError> {
        let request = self.client.get_object().bucket(&self.bucket).key(key);
        let deadline = time::Instant::now() + self.timeout;
        let response = match time::timeout_at(deadline, request.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => return Err(GarageError::Get),
        };
        if response
            .content_length()
            .and_then(|len| u64::try_from(len).ok())
            .is_some_and(|len| len > self.max_bytes)
        {
            return Err(GarageError::TooLarge);
        }
        let mut body = response.body;
        let mut out = Vec::new();
        loop {
            let next = match time::timeout_at(deadline, body.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(_))) | Err(_) => return Err(GarageError::Get),
                Ok(None) => break,
            };
            if out.len() as u64 + next.len() as u64 > self.max_bytes {
                return Err(GarageError::TooLarge);
            }
            out.extend_from_slice(&next);
        }
        if out.is_empty() {
            return Err(GarageError::Get);
        }
        Ok(out)
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
