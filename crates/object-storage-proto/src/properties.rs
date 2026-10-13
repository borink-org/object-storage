//! The properties that a listing reports for each entry, as a set, and the
//! values that one entry gave for them.
//!
//! Each service names its own properties, and has its own aliases of these
//! types: [`azure::PropertySet`](crate::azure::PropertySet) of
//! [`azure::BlobProperty`](crate::azure::BlobProperty), and
//! [`s3::PropertySet`](crate::s3::PropertySet) of
//! [`s3::ObjectProperty`](crate::s3::ObjectProperty). Use those.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

/// A property that a listing can report for each entry: an
/// [`azure::BlobProperty`](crate::azure::BlobProperty) or an
/// [`s3::ObjectProperty`](crate::s3::ObjectProperty).
pub trait Property: Copy + sealed::Sealed {
    /// How many properties there are, which is the most a set can hold.
    const COUNT: usize;

    /// Returns the property's number, below [`Self::COUNT`].
    fn number(self) -> u8;
}

pub(crate) mod sealed {
    // Only this crate's property types are properties: a set's bits are
    // numbered by the order that the page readers match them in.
    pub trait Sealed {}
}

/// The properties that one read of a page is asked for.
///
/// Build one with `of`, such as
/// [`azure::PropertySet::of`](crate::azure::PropertySet::of), and pass it to
/// the service's `fill_listing_with`. The values come back in the order
/// that the property type numbers them, whatever order the set was built
/// in.
pub struct PropertySet<P> {
    bits: u64,
    property: PhantomData<fn() -> P>,
}

impl<P: Property> PropertySet<P> {
    pub(crate) const fn new(bits: u64) -> Self {
        Self {
            bits,
            property: PhantomData,
        }
    }

    /// A set from its bits, one per property in the order that the property
    /// type numbers them. A bit that names no property is dropped.
    pub const fn from_bits(bits: u64) -> Self {
        // Each property type checks that its count is below 64, so this
        // shift leaves a nonzero value.
        Self::new(bits & ((1 << P::COUNT) - 1))
    }

    /// The set's bits, as [`Self::from_bits`] reads them.
    pub const fn bits(self) -> u64 {
        self.bits
    }

    /// Whether the set holds this property.
    pub fn contains(self, property: P) -> bool {
        self.bits & (1 << property.number()) != 0
    }

    /// How many properties the set holds, which is how many values a read
    /// reports for each entry.
    pub const fn len(self) -> usize {
        self.bits.count_ones() as usize
    }

    /// Whether the set holds nothing.
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Where a property's value stands among the values of an entry: its
    /// rank among the set's members, in the order that the property type
    /// numbers them. Meaningful only for a property the set holds.
    pub fn slot(self, property: P) -> usize {
        (self.bits & ((1 << property.number()) - 1)).count_ones() as usize
    }
}

// The set holds bits alone, so it is copied, compared and hashed as its
// bits, whatever the property type implements.
impl<P> Clone for PropertySet<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for PropertySet<P> {}

impl<P> Default for PropertySet<P> {
    fn default() -> Self {
        Self {
            bits: 0,
            property: PhantomData,
        }
    }
}

impl<P> PartialEq for PropertySet<P> {
    fn eq(&self, other: &Self) -> bool {
        self.bits == other.bits
    }
}

impl<P> Eq for PropertySet<P> {}

impl<P> Hash for PropertySet<P> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bits.hash(state);
    }
}

impl<P> fmt::Debug for PropertySet<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PropertySet").field(&self.bits).finish()
    }
}

/// The values that one entry gave for the properties of a set.
///
/// The service's `fill_listing_with` hands one to the closure that builds
/// each entry. Each value is the bytes between the element's tags, as the
/// service wrote them, under the rules that
/// [`ListEntry::property`](crate::ListEntry::property) states. A group of
/// keys gives no values.
pub struct PropertyValues<'x, 'b, P> {
    set: PropertySet<P>,
    values: &'x [Option<&'b [u8]>],
}

impl<'x, 'b, P: Property> PropertyValues<'x, 'b, P> {
    pub(crate) fn new(set: PropertySet<P>, values: &'x [Option<&'b [u8]>]) -> Self {
        Self { set, values }
    }

    /// The set that the page was read with.
    pub const fn set(&self) -> PropertySet<P> {
        self.set
    }

    /// The value the entry gave for one property.
    ///
    /// [`None`] if the property is not in the set or the entry wrote no such
    /// element; an empty slice if it wrote the element empty.
    pub fn get(&self, property: P) -> Option<&'b [u8]> {
        if !self.set.contains(property) {
            return None;
        }
        self.values[self.set.slot(property)]
    }

    /// Every value, one per member of the set, in the order that the
    /// property type numbers them.
    pub const fn all(&self) -> &'x [Option<&'b [u8]>] {
        self.values
    }
}

impl<P> Clone for PropertyValues<'_, '_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for PropertyValues<'_, '_, P> {}

impl<P> fmt::Debug for PropertyValues<'_, '_, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PropertyValues")
            .field("set", &self.set)
            .field("values", &self.values)
            .finish()
    }
}
