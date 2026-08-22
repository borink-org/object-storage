use borink_object_storage::{Blobs, CapacityError, Container, Extent, RequestWorkspace};

struct VecExtent(Vec<u8>);

impl Extent for VecExtent {
    fn as_slice(&self) -> &[u8] {
        &self.0
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.0
    }

    fn try_reserve(&mut self, required: usize) -> bool {
        self.0.resize(required, 0);
        true
    }
}

#[test]
fn the_host_can_grow_an_extent_and_retry() {
    let blobs = Blobs::new(
        Container::new("https://account", "container").unwrap(),
        "token",
    )
    .unwrap();
    let mut extent = VecExtent(Vec::new());
    let mut workspace = RequestWorkspace::with_extent(&mut extent);
    let error = blobs
        .get_request(&mut workspace, "object", "Sat, 22 Aug 2026 12:00:00 GMT")
        .unwrap_err();
    let borink_object_storage::Error::Capacity(capacity) = error else {
        panic!("unexpected error: {error}");
    };
    assert!(workspace.try_reserve(capacity));
    assert!(
        blobs
            .get_request(&mut workspace, "object", "Sat, 22 Aug 2026 12:00:00 GMT",)
            .is_ok()
    );
}

#[test]
fn fixed_slices_can_refuse_growth() {
    let mut bytes = [0; 8];
    let mut workspace = RequestWorkspace::new(&mut bytes);
    assert!(!workspace.try_reserve(CapacityError {
        extent: borink_object_storage::WorkspaceExtent::Packed,
        required: 9,
        available: 8,
    }));
}
