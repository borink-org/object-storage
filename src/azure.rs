use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::request::{Writer, text};
use crate::{
    CapacityError, Error, Request, RequestRequirements, RequestWorkspace, Response, Result,
    WorkspaceExtent,
};

pub const VERSION: &str = "2023-11-03";

const PATH_ESCAPE: &AsciiSet = &CONTROLS
    .add(b':')
    .add(b'?')
    .add(b'#')
    .add(b'[')
    .add(b']')
    .add(b'@')
    .add(b'!')
    .add(b'$')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b';')
    .add(b'=')
    .add(b'"')
    .add(b' ')
    .add(b'<')
    .add(b'>')
    .add(b'%')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'`');

#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    endpoint: &'a str,
    name: &'a str,
}

impl<'a> Container<'a> {
    pub fn new(endpoint: &'a str, name: &'a str) -> Result<Self> {
        let Some((scheme, authority)) = endpoint.split_once("://") else {
            return Err(Error::InvalidEndpoint);
        };
        if !matches!(scheme, "http" | "https")
            || authority.is_empty()
            || authority
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#' | b'@'))
        {
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
    pub fn new(container: Container<'a>, token: &'a str) -> Result<Self> {
        if !valid_header(token) {
            return Err(Error::InvalidToken);
        }
        Ok(Self { container, token })
    }

    pub fn get_request<'request>(
        &self,
        workspace: &'request mut RequestWorkspace<'_>,
        key: &str,
        date: &'request str,
    ) -> Result<Request<'request>> {
        validate_get(key, date)?;
        let required = self.get_request_requirements(key, date)?.packed;
        let available = workspace.capacity();
        if required > available {
            return Err(CapacityError {
                extent: WorkspaceExtent::Packed,
                required,
                available,
            }
            .into());
        }

        let mut out = Writer::storing(&mut workspace.bytes()[..required]);
        let url_end = self.build(&mut out, key);
        let bytes = out.finish().expect("storing writer");
        Ok(Request::new(
            text(&bytes[..url_end]),
            text(&bytes[url_end..]),
            date,
            VERSION,
        ))
    }

    pub fn get_request_requirements(&self, key: &str, date: &str) -> Result<RequestRequirements> {
        validate_get(key, date)?;
        let mut out = Writer::counting();
        self.build(&mut out, key);
        Ok(RequestRequirements {
            packed: out.position(),
        })
    }

    fn build(&self, out: &mut Writer<'_>, key: &str) -> usize {
        out.push(self.container.endpoint);
        out.push("/");
        out.push(self.container.name);
        out.push("/");
        for part in utf8_percent_encode(key, PATH_ESCAPE) {
            out.push(part);
        }
        let url_end = out.position();
        out.push("Bearer ");
        out.push(self.token);
        url_end
    }

    pub fn interpret_get<'response>(
        &self,
        response: Response<'response>,
    ) -> Result<&'response [u8]> {
        match response.status() {
            200..=299 => Ok(response.body()),
            404 => Err(Error::NotFound),
            401 | 403 => Err(Error::Unauthorized),
            status => Err(Error::Status(status)),
        }
    }
}

fn validate_get(key: &str, date: &str) -> Result<()> {
    if key.is_empty() || key.chars().count() > 1024 {
        return Err(Error::InvalidKey);
    }
    if !valid_header(date) {
        return Err(Error::InvalidDate);
    }
    Ok(())
}

fn valid_header(value: &str) -> bool {
    !value.is_empty() && value.is_ascii() && !value.bytes().any(|byte| byte.is_ascii_control())
}
