//! Azure requests, written from templates.

use borink_object_storage_proto::azure::{self, Template};
use borink_object_storage_proto::azure::{AzureNamespace, Container};
use borink_object_storage_proto::{
    DeleteHeadOutcome, DeleteOptions, Error, GetHeadOutcome, GetItem, GetOptions, ListEntry,
    ListHeadOutcome, ListItem, ListOptions, Listing, ObjectItem, Payload, PutHeadOutcome, PutItem,
    PutOptions,
};

use crate::Prepared;

/// A client of one Azure container, with one bearer token.
///
/// It writes the template of each kind of request once, when it is made,
/// and writes every request into one buffer that it keeps. Once that buffer
/// has grown to fit the longest request, each request is written once. It
/// sends with one `ureq` agent, which keeps its connections.
pub struct Blobs {
    get: Prepared<Template>,
    put: Prepared<Template>,
    delete: Prepared<Template>,
    list: Prepared<Template>,
    buf: Vec<u8>,
    agent: ureq::Agent,
}

impl Blobs {
    /// A client of `container` that sends `token`, and lists as `list`
    /// says.
    ///
    /// # Errors
    ///
    /// Returns the error of a template that the crate refuses to write.
    pub fn new(
        container: &Container<'_>,
        token: &str,
        list: &ListOptions<'_>,
    ) -> Result<Self, Error> {
        let namespace = AzureNamespace::Unknown;
        Ok(Self {
            get: Prepared::new(|buf| {
                azure::prepare_get(container, token, namespace, &GetOptions::new(), buf)
            })?,
            put: Prepared::new(|buf| {
                azure::prepare_put(container, token, namespace, &PutOptions::default(), buf)
            })?,
            delete: Prepared::new(|buf| {
                azure::prepare_delete(container, token, namespace, &DeleteOptions::new(), buf)
            })?,
            list: Prepared::new(|buf| azure::prepare_list(container, token, namespace, list, buf))?,
            buf: Vec::new(),
            agent: crate::new_agent(),
        })
    }

    /// Gets the object `key`, and returns its bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if Azure
    /// returned no object.
    pub fn get(&mut self, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let now = crate::now()?;
        let item = GetItem::new(key);
        let Prepared { template, bytes } = &self.get;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            azure::encode_get(template, bytes, &item, &now, buf)
        })?;

        let (parts, mut body) = crate::send(&self.agent, &placed.request(&self.buf), None)?;
        let shape = item.shape(&GetOptions::new());
        match azure::accept_get_head(shape, crate::response_head(&parts))? {
            GetHeadOutcome::Body { .. } => body.read_to_vec().map_err(Into::into),
            GetHeadOutcome::Complete { .. } => Ok(Vec::new()),
            // Azure named no error in the head, so the body names it.
            GetHeadOutcome::NeedErrorBody(failure) => Err(no_object(azure::accept_get_error_body(
                shape,
                failure,
                &crate::error_body(&mut body),
            ))),
            outcome => Err(no_object(outcome)),
        }
    }

    /// Puts `content` as the whole object `key`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if Azure
    /// stored no object.
    pub fn put(&mut self, key: &str, content: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let now = crate::now()?;
        let item = PutItem::new(key, Payload::Slice(content));
        let Prepared { template, bytes } = &self.put;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            azure::encode_put(template, bytes, &item, &now, buf)
        })?;

        let request = placed.request(&self.buf);
        let (parts, mut body) = crate::send(&self.agent, &request, Some(content))?;
        let shape = item.shape(&PutOptions::default());
        match azure::accept_put_head(shape, crate::response_head(&parts))? {
            PutHeadOutcome::Created { .. } => Ok(()),
            PutHeadOutcome::NeedErrorBody(failure) => Err(not_stored(
                azure::accept_put_error_body(shape, failure, &crate::error_body(&mut body)),
            )),
            outcome => Err(not_stored(outcome)),
        }
    }

    /// Deletes the whole object `key`.
    ///
    /// Reports a missing object rather than treating it as success: only the
    /// caller knows whether it meant to remove an object that is already
    /// gone.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if Azure
    /// removed no object.
    pub fn delete(&mut self, key: &str) -> Result<(), Box<dyn std::error::Error>> {
        let now = crate::now()?;
        let item = ObjectItem::new(key);
        let Prepared { template, bytes } = &self.delete;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            azure::encode_delete(template, bytes, &item, &now, buf)
        })?;

        let (parts, mut body) = crate::send(&self.agent, &placed.request(&self.buf), None)?;
        let shape = DeleteOptions::new().shape();
        match azure::accept_delete_head(shape, crate::response_head(&parts))? {
            DeleteHeadOutcome::Accepted => Ok(()),
            DeleteHeadOutcome::NeedErrorBody(failure) => Err(not_removed(
                azure::accept_delete_error_body(shape, failure, &crate::error_body(&mut body)),
            )),
            outcome => Err(not_removed(outcome)),
        }
    }

    /// Lists one page, and reads it into `body`.
    ///
    /// The entries this writes into `into` borrow `body`. An array of the
    /// `max_results` of the client's list options always holds a whole page.
    /// A smaller one is refused with the number of entries the page holds.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if Azure listed
    /// nothing.
    pub fn list<'b>(
        &mut self,
        item: &ListItem<'_>,
        body: &'b mut Vec<u8>,
        into: &mut [ListEntry<'b>],
    ) -> Result<Listing<'b>, Box<dyn std::error::Error>> {
        let now = crate::now()?;
        let Prepared { template, bytes } = &self.list;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            azure::encode_list(template, bytes, item, &now, buf)
        })?;

        let (parts, mut incoming) = crate::send(&self.agent, &placed.request(&self.buf), None)?;
        match azure::accept_list_head(crate::response_head(&parts))? {
            ListHeadOutcome::Page { .. } => {
                // The whole page, in one buffer. The entries point into it,
                // and reading it decodes the text where it stands.
                *body = crate::page_body(&mut incoming)?;
                azure::fill_listing(body, into).map_err(Into::into)
            }
            ListHeadOutcome::NeedErrorBody(failure) => Err(not_listed(
                azure::accept_list_error_body(failure, &crate::error_body(&mut incoming)),
            )),
            outcome => Err(not_listed(outcome)),
        }
    }
}

fn no_object(outcome: GetHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure returned no object: {outcome}").into()
}

fn not_stored(outcome: PutHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure stored no object: {outcome}").into()
}

fn not_removed(outcome: DeleteHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure removed no object: {outcome}").into()
}

fn not_listed(outcome: ListHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure listed no keys: {outcome}").into()
}
