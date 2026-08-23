//! Caller-defined extent integration tests.

#[cfg(feature = "alloc")]
use borink_object_storage::{Blobs, Container, RequestWorkspace, Timestamps, VecExtent};

#[cfg(feature = "alloc")]
#[test]
fn host_can_grow_the_request_extent() {
    let blobs = Blobs::new(
        Container::new("https://account.blob.core.windows.net", "objects").unwrap(),
        "token",
    )
    .unwrap();
    let now = Timestamps::from_unix(1_787_400_000);
    let mut extent = VecExtent::new();
    let mut workspace = RequestWorkspace::with_extent(&mut extent);

    let error = blobs
        .get_request(&mut workspace, "object", &now)
        .unwrap_err();
    assert!(workspace.try_reserve(error.capacity().unwrap()));
    blobs.get_request(&mut workspace, "object", &now).unwrap();
}
