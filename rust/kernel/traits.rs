// SPDX-License-Identifier: GPL-2.0

//! Traits useful to drivers, and their implementations for common types.

use core::{ops::Deref, pin::Pin};

use alloc::collections::TryReserveError;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::{alloc::AllocError, sync::Arc};

type AllocResult<T = ()> = core::result::Result<T, AllocError>;
type CollectionResult<T = ()> = core::result::Result<T, TryReserveError>;

#[inline]
fn assume_fallible<T, F: FnOnce() -> T>(f: F) -> T {
    f()
}

/// Trait which provides a fallible version of `pin()` for pointer types.
///
/// Common pointer types which implement a `pin()` method include [`Box`](alloc::boxed::Box) and [`Arc`].
pub trait TryPin<P: Deref> {
    /// Constructs a new `Pin<pointer<T>>`. If `T` does not implement [`Unpin`], then data
    /// will be pinned in memory and unable to be moved. An error will be returned
    /// if allocation fails.
    fn try_pin(data: P::Target) -> core::result::Result<Pin<P>, AllocError>;
}

impl<T> TryPin<Arc<T>> for Arc<T> {
    fn try_pin(data: T) -> core::result::Result<Pin<Arc<T>>, AllocError> {
        // SAFETY: the data `T` is exposed only through a `Pin<Arc<T>>`, which
        // does not allow data to move out of the `Arc`. Therefore it can
        // never be moved.
        Ok(unsafe { Pin::new_unchecked(Arc::try_new(data)?) })
    }
}

/// Faillible alternative to [`alloc::borrow::ToOwned`].
pub trait TryToOwned {
    /// The resulting type after obtaining ownership.
    type Owned: core::borrow::Borrow<Self>;

    /// Faillible alternative to [`alloc::borrow::ToOwned::to_owned`].
    #[must_use = "cloning is often expensive and is not expected to have side effects"]
    fn try_to_owned(&self) -> AllocResult<Self::Owned>;

    /// Faillible alternative to [`alloc::borrow::ToOwned::clone_into`].
    fn try_clone_into(&self, target: &mut Self::Owned) -> AllocResult {
        *target = self.try_to_owned()?;
        Ok(())
    }
}

impl<T: Clone> TryToOwned for [T] {
    type Owned = Vec<T>;

    fn try_to_owned(&self) -> AllocResult<Vec<T>> {
        let mut vec = Vec::new();
        self.try_clone_into(&mut vec)?;
        Ok(vec)
    }

    fn try_clone_into(&self, target: &mut Vec<T>) -> AllocResult {
        // Ensure target has enough capacity
        target
            .try_reserve_exact(self.len().saturating_sub(target.len()))
            .map_err(|_| AllocError)?;

        target.clear();
        assume_fallible(|| target.extend_from_slice(self));
        Ok(())
    }
}

impl TryToOwned for str {
    type Owned = String;

    fn try_to_owned(&self) -> AllocResult<String> {
        let mut vec = String::new();
        self.try_clone_into(&mut vec)?;
        Ok(vec)
    }

    fn try_clone_into(&self, target: &mut String) -> AllocResult {
        // Ensure target has enough capacity
        target
            .try_reserve_exact(self.len().saturating_sub(target.len()))
            .map_err(|_| AllocError)?;

        target.clear();
        assume_fallible(|| target.push_str(self));
        Ok(())
    }
}

/// Trait which provides a fallible methods for [`Vec`].
pub trait VecExt<T> {
    /// Faillible alternative to [`Vec::with_capacity`].
    fn try_with_capacity(capacity: usize) -> CollectionResult<Self>
    where
        Self: Sized;

    /// Faillible alternative to [`Vec::extend_from_slice`].
    fn try_extend_from_slice(&mut self, other: &[T]) -> CollectionResult
    where
        T: Clone;

    /// Faillible alternative to [`Vec::resize`].
    fn try_resize(&mut self, new_len: usize, value: T) -> CollectionResult
    where
        T: Clone;
}

impl<T> VecExt<T> for alloc::vec::Vec<T> {
    fn try_with_capacity(capacity: usize) -> CollectionResult<Self> {
        let mut vec = Self::new();
        vec.try_reserve_exact(capacity)?;
        Ok(vec)
    }

    fn try_extend_from_slice(&mut self, other: &[T]) -> CollectionResult
    where
        T: Clone,
    {
        self.try_reserve(other.len())?;
        assume_fallible(|| self.extend_from_slice(other));
        Ok(())
    }

    fn try_resize(&mut self, new_len: usize, value: T) -> CollectionResult
    where
        T: Clone,
    {
        self.try_reserve(new_len.saturating_sub(self.len()))?;
        assume_fallible(|| self.resize(new_len, value));
        Ok(())
    }
}
