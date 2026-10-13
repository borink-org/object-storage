//! Azure requests, written from templates.

use borink_object_storage_proto::azure::{self, Template};
use borink_object_storage_proto::azure::{AzureNamespace, Container};
use borink_object_storage_proto::{
    CapacityError, DeleteHeadOutcome, DeleteOptions, GetHeadOutcome, GetItem, GetOptions,
    ListEntry, ListHeadOutcome, ListItem, Listing, ObjectItem, Payload, PutHeadOutcome, PutItem,
    PutOptions,
};

/// Builds and executes one GET request, returning an owned response body.
///
/// The template is written for this one read. A host that reads many
/// objects writes it once and keeps it.
pub fn get(
    container: &Container<'_>,
    token: &str,
    key: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let options = GetOptions::new();
    let prepare = |buf: &mut [u8]| {
        azure::prepare_get(container, token, AzureNamespace::Unknown, &options, buf)
    };
    let (template, bytes) = crate::sized_template(prepare)?;
    let item = GetItem::new(key);
    let encode = |buf: &mut [u8]| azure::encode_get(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; CapacityError::required_by(encode)?];
    let request = azure::encode_get(&template, &bytes, &item, &now, &mut buf)?;

    let (parts, mut body) = crate::send(&request, None)?;
    let shape = item.shape(&options);
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

fn no_object(outcome: GetHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure returned no object: {outcome}").into()
}

/// Builds and executes one PUT request, storing `content` as the whole object.
pub fn put(
    container: &Container<'_>,
    token: &str,
    key: &str,
    content: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let options = PutOptions::default();
    let prepare = |buf: &mut [u8]| {
        azure::prepare_put(container, token, AzureNamespace::Unknown, &options, buf)
    };
    let (template, bytes) = crate::sized_template(prepare)?;
    let item = PutItem::new(key, Payload::Slice(content));
    let encode = |buf: &mut [u8]| azure::encode_put(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; CapacityError::required_by(encode)?];
    let request = azure::encode_put(&template, &bytes, &item, &now, &mut buf)?;

    let (parts, mut body) = crate::send(&request, None)?;
    let shape = item.shape(&options);
    match azure::accept_put_head(shape, crate::response_head(&parts))? {
        PutHeadOutcome::Created { .. } => Ok(()),
        PutHeadOutcome::NeedErrorBody(failure) => Err(not_stored(azure::accept_put_error_body(
            shape,
            failure,
            &crate::error_body(&mut body),
        ))),
        outcome => Err(not_stored(outcome)),
    }
}

fn not_stored(outcome: PutHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure stored no object: {outcome}").into()
}

/// Builds and executes one DELETE request, removing the whole object.
///
/// Reports a missing object rather than treating it as success: only the
/// caller knows whether it meant to remove an object that is already gone.
pub fn delete(
    container: &Container<'_>,
    token: &str,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let options = DeleteOptions::new();
    let prepare = |buf: &mut [u8]| {
        azure::prepare_delete(container, token, AzureNamespace::Unknown, &options, buf)
    };
    let (template, bytes) = crate::sized_template(prepare)?;
    let item = ObjectItem::new(key);
    let encode =
        |buf: &mut [u8]| azure::encode_delete(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; CapacityError::required_by(encode)?];
    let request = azure::encode_delete(&template, &bytes, &item, &now, &mut buf)?;

    let (parts, mut body) = crate::send(&request, None)?;
    let shape = options.shape();
    match azure::accept_delete_head(shape, crate::response_head(&parts))? {
        DeleteHeadOutcome::Accepted => Ok(()),
        DeleteHeadOutcome::NeedErrorBody(failure) => Err(not_removed(
            azure::accept_delete_error_body(shape, failure, &crate::error_body(&mut body)),
        )),
        outcome => Err(not_removed(outcome)),
    }
}

fn not_removed(outcome: DeleteHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure removed no object: {outcome}").into()
}

/// Builds and executes one listing request from `template`, and reads the
/// page it answered.
///
/// Write the template once with `azure::prepare_list` and pass it, with its
/// bytes, for every page. This function reads the page into `body`, and
/// the entries it writes into `into` borrow those bytes. An array of
/// `max_results` entries always holds a whole page. A smaller one is
/// refused with the number of entries the page holds.
///
/// # Errors
///
/// Returns an error if the request could not be sent, or if Azure listed
/// nothing.
pub fn list<'b>(
    template: &Template,
    bytes: &[u8],
    item: &ListItem<'_>,
    body: &'b mut Vec<u8>,
    into: &mut [ListEntry<'b>],
) -> Result<Listing<'b>, Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let encode = |buf: &mut [u8]| azure::encode_list(template, bytes, item, &now, buf).map(drop);
    let mut buf = vec![0; CapacityError::required_by(encode)?];
    let request = azure::encode_list(template, bytes, item, &now, &mut buf)?;

    let (parts, mut incoming) = crate::send(&request, None)?;
    match azure::accept_list_head(crate::response_head(&parts))? {
        ListHeadOutcome::Page { .. } => {
            // The whole page, in one buffer. The entries point into it, and
            // reading it decodes the text where it stands.
            *body = crate::page_body(&mut incoming)?;
            azure::fill_listing(body, into).map_err(Into::into)
        }
        ListHeadOutcome::NeedErrorBody(failure) => Err(not_listed(azure::accept_list_error_body(
            failure,
            &crate::error_body(&mut incoming),
        ))),
        outcome => Err(not_listed(outcome)),
    }
}

fn not_listed(outcome: ListHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure listed no keys: {outcome}").into()
}
