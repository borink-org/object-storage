use crate::request::{Writer, text};
use crate::{
    CapacityError, Error, Request, RequestRequirements, RequestWorkspace, Response, Result,
    Timestamps, WorkspaceExtent,
};

/// Latest Azure Storage version fully deployed in every region.
///
/// See the [Azure Storage service version lifecycle](https://learn.microsoft.com/en-us/rest/api/storageservices/versioning-for-the-azure-storage-services).
pub const VERSION: &str = "2026-04-06";

// Azure limits blob names to 1,024 characters.
const MAX_BLOB_NAME_CHARS: usize = 1024;

/// Borrowed Azure Blob endpoint and container configuration.
#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    endpoint: &'a str,
    name: &'a str,
}

impl<'a> Container<'a> {
    /// Validates and borrows an HTTP(S) origin and container name.
    pub fn new(endpoint: &'a str, name: &'a str) -> Result<Self> {
        if !crate::http::valid_http_origin(endpoint) {
            return Err(Error::InvalidEndpoint);
        }
        if name.is_empty()
            || name
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control())
        {
            return Err(Error::InvalidContainer);
        }
        Ok(Self { endpoint, name })
    }
}

/// Azure Blob operations authorized by a borrowed bearer token.
#[derive(Clone, Copy)]
pub struct Blobs<'a> {
    container: Container<'a>,
    token: &'a str,
}

impl core::fmt::Debug for Blobs<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Blobs")
            .field("container", &self.container)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl<'a> Blobs<'a> {
    /// Validates and borrows a container and bearer token.
    pub fn new(container: Container<'a>, token: &'a str) -> Result<Self> {
        if !valid_header(token) {
            return Err(Error::InvalidToken);
        }
        Ok(Self { container, token })
    }

    /// Builds a whole-object GET request in `workspace`.
    ///
    /// A capacity error reports the exact packed extent size; the host may grow
    /// that extent and retry the same call.
    pub fn get_request<'request>(
        &self,
        workspace: &'request mut RequestWorkspace<'_>,
        key: &str,
        now: &'request Timestamps,
    ) -> Result<Request<'request>> {
        validate_key(key)?;
        let available = workspace.capacity();
        // The storing writer keeps counting after capacity is exhausted. One
        // pass therefore produces either the request or its exact requirement.
        let mut out = Writer::storing(workspace.bytes());
        let url_end = self.build(&mut out, key);
        let required = out.position();
        if required > available {
            return Err(CapacityError {
                extent: WorkspaceExtent::Packed,
                required,
                available,
            }
            .into());
        }
        let bytes = out.finish().expect("capacity was checked");
        Ok(Request::new(
            text(&bytes[..url_end]),
            text(&bytes[url_end..]),
            now.rfc1123(),
            VERSION,
        ))
    }

    /// Measures the packed extent required by [`Self::get_request`].
    pub fn get_request_requirements(&self, key: &str) -> Result<RequestRequirements> {
        validate_key(key)?;
        let mut out = Writer::counting();
        self.build(&mut out, key);
        Ok(RequestRequirements {
            packed: out.position(),
        })
    }

    fn build(&self, out: &mut Writer<'_>, key: &str) -> usize {
        // URL and authorization text share one packed extent. `url_end` splits
        // the two borrowed strings after construction without another buffer.
        out.push(self.container.endpoint);
        out.push("/");
        out.push(self.container.name);
        out.push("/");
        for part in crate::path::encode_object_key(key) {
            out.push(part);
        }
        let url_end = out.position();
        out.push("Bearer ");
        out.push(self.token);
        url_end
    }

    /// Interprets GET response metadata before the host reads the body.
    pub fn interpret_get(&self, response: Response) -> Result<()> {
        match response.status() {
            200..=299 => Ok(()),
            404 => Err(Error::NotFound),
            401 | 403 => Err(Error::Unauthorized),
            status => Err(Error::Status(status)),
        }
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.chars().count() > MAX_BLOB_NAME_CHARS {
        return Err(Error::InvalidKey);
    }
    Ok(())
}

fn valid_header(value: &str) -> bool {
    !value.is_empty() && value.is_ascii() && !value.bytes().any(|byte| byte.is_ascii_control())
}
