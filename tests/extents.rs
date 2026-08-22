use borink_object_storage::{Blobs, Container, Extent, RequestWorkspace, Timestamps};

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
fn host_can_grow_the_request_extent() {
    let blobs = Blobs::new(
        Container::new("https://account.blob.core.windows.net", "objects").unwrap(),
        "token",
    )
    .unwrap();
    let now = Timestamps::from_unix(1_787_400_000);
    let mut extent = VecExtent(Vec::new());
    let mut workspace = RequestWorkspace::with_extent(&mut extent);

    let error = blobs
        .get_request(&mut workspace, "object", &now)
        .unwrap_err();
    assert!(workspace.try_reserve(error.capacity().unwrap()));
    let request = blobs.get_request(&mut workspace, "object", &now).unwrap();

    assert_eq!(request.method(), "GET");
}

#[test]
fn reports_requirements_without_a_workspace() {
    let blobs = Blobs::new(
        Container::new("https://account", "objects").unwrap(),
        "token",
    )
    .unwrap();
    let requirements = blobs.get_request_requirements("a key").unwrap();
    let mut storage = vec![0; requirements.packed];
    let mut workspace = RequestWorkspace::new(&mut storage);

    blobs
        .get_request(&mut workspace, "a key", &Timestamps::from_unix(0))
        .unwrap();
}
