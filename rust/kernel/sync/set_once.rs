// SPDX-License-Identifier: GPL-2.0

//! A container that can be initialized at most once.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit, //
};
use pin_init::{
    Init,
    PinInit, //
};

use super::atomic::{
    ordering::{
        Acquire,
        Release, //
    },
    Atomic, //
};
use crate::prelude::*;

/// A container that can be populated at most once. Thread safe.
///
/// Once the a [`SetOnce`] is populated, it remains populated by the same object for the
/// lifetime `Self`.
///
/// # Invariants
///
/// - `init` may only assume values in the range `0..=2`.
/// - `init == 0` if and only if `value` is uninitialized.
/// - `init == 1` if and only if there is exactly one thread with exclusive
///   access to `self.value`.
/// - `init == 2` if and only if `value` is initialized and valid for shared
///   access.
/// - once `init == 2`, it must remain so.
///
/// # Example
///
/// ```
/// # use kernel::sync::SetOnce;
/// let value = SetOnce::new();
/// assert_eq!(None, value.as_ref());
///
/// let status = value.populate(42u8);
/// assert_eq!(true, status);
/// assert_eq!(Some(&42u8), value.as_ref());
/// assert_eq!(Some(42u8), value.copy());
///
/// let status = value.populate(101u8);
/// assert_eq!(false, status);
/// assert_eq!(Some(&42u8), value.as_ref());
/// assert_eq!(Some(42u8), value.copy());
/// ```
pub struct SetOnce<T> {
    init: Atomic<u32>,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Default for SetOnce<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Error that can occur during initialization of `SetOnce`.
#[derive(Debug)]
pub enum InitError<E> {
    /// The `Once` has already been initialized.
    AlreadyInit,
    /// The `Once` is being raced to initialize by another thread.
    RacedInit,
    /// Error occurs during initialization.
    DuringInit(E),
}

impl<E> From<E> for InitError<E> {
    #[inline]
    fn from(err: E) -> Self {
        InitError::DuringInit(err)
    }
}

impl<E: Into<Error>> From<InitError<E>> for Error {
    #[inline]
    fn from(this: InitError<E>) -> Self {
        match this {
            InitError::AlreadyInit => EEXIST,
            InitError::RacedInit => EBUSY,
            InitError::DuringInit(e) => e.into(),
        }
    }
}

impl<T> SetOnce<T> {
    /// Create a new [`SetOnce`].
    ///
    /// The returned instance will be empty.
    pub const fn new() -> Self {
        // INVARIANT: The container is empty and we initialize `init` to `0`.
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            init: Atomic::new(0),
        }
    }

    /// Get a reference to the contained object.
    ///
    /// Returns [`None`] if this [`SetOnce`] is empty.
    pub fn as_ref(&self) -> Option<&T> {
        if self.init.load(Acquire) == 2 {
            // SAFETY: By the type invariants of `Self`, `self.init == 2` means that `self.value`
            // is initialized and valid for shared access.
            Some(unsafe { &*self.value.get().cast() })
        } else {
            None
        }
    }

    /// Populate the [`SetOnce`] with an initializer.
    ///
    /// Returns the initialized reference if the [`SetOnce`] was successfully populated.
    pub fn init<E>(&self, init: impl Init<T, E>) -> Result<&T, InitError<E>> {
        // INVARIANT: If the swap succeeds:
        //  - We write the valid value `1` to `init`.
        //  - The previous value is not `2`, so it is valid to move to `1`.
        //  - Only one thread can succeed in this write, so we have exclusive access after the
        //    write.
        match self.init.cmpxchg(0, 1, Acquire) {
            Ok(_) => {
                // SAFETY:
                // - By the type invariants of `Self`, the fact that we succeeded in writing `1`
                //   to `self.init` means we obtained exclusive access to `self.value`.
                // - When `Err` is returned, we did not set `self.init` to `2` so the `Drop` is not
                //   armed.
                match unsafe { init.__init(self.value.get().cast()) } {
                    Ok(()) => {
                        // INVARIANT:
                        //  - The previous value is `1`, so it is valid to move to `2`.
                        //  - We write the valid value `2` to `init`.
                        //  - We release our exclusive access to `self.value` and it is now valid for shared
                        //    access.
                        self.init.store(2, Release);
                        // SAFETY: we have just initialized the value.
                        Ok(unsafe { &*self.value.get().cast() })
                    }
                    Err(err) => {
                        // INVARIANT:
                        //  - The previous value is `1`, so it is valid to move to `0`.
                        //  - We write the valid value `0` to `init`.
                        //  - We release our exclusive access to `self.value` and it is now valid for shared
                        //    access.
                        self.init.store(0, Release);
                        Err(err.into())
                    }
                }
            }
            Err(1) => Err(InitError::RacedInit),
            Err(_) => Err(InitError::AlreadyInit),
        }
    }

    /// Populate the [`SetOnce`] with a pinned initializer.
    ///
    /// Returns the initialized reference if the [`SetOnce`] was successfully populated.
    pub fn pin_init<E>(self: Pin<&Self>, init: impl PinInit<T, E>) -> Result<&T, InitError<E>> {
        // SAFETY:
        // - `__pinned_init` satisfy all requirements of `init_from_closure`
        // - calling `__pinned_init` require additional that the slot is pinned, which is satisfied given `self: Pin<&Self>`.
        self.get_ref()
            .init(unsafe { pin_init::init_from_closure(|slot| init.__pinned_init(slot)) })
    }

    /// Populate the [`SetOnce`].
    ///
    /// Returns `true` if the [`SetOnce`] was successfully populated.
    pub fn populate(&self, value: T) -> bool {
        self.init(value).is_ok()
    }

    /// Get a copy of the contained object.
    ///
    /// Returns [`None`] if the [`SetOnce`] is empty.
    pub fn copy(&self) -> Option<T>
    where
        T: Copy,
    {
        self.as_ref().copied()
    }
}

impl<T> Drop for SetOnce<T> {
    fn drop(&mut self) {
        if *self.init.get_mut() == 2 {
            let value = self.value.get_mut();
            // SAFETY: By the type invariants of `Self`, `self.init == 2` means that `self.value`
            // contains a valid value. We have exclusive access, as we hold a `mut` reference to
            // `self`.
            unsafe { value.assume_init_drop() };
        }
    }
}

// SAFETY: `SetOnce` can be transferred across thread boundaries iff the data it contains can.
unsafe impl<T: Send> Send for SetOnce<T> {}

// SAFETY: `SetOnce` synchronises access to the inner value via atomic operations,
// so shared references are safe when `T: Sync`. Since the inner `T` may be dropped
// on any thread, we also require `T: Send`.
unsafe impl<T: Send + Sync> Sync for SetOnce<T> {}
