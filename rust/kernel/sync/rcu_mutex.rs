// SPDX-License-Identifier: GPL-2.0

//! A kernel mutex.
//!
//! This module allows Rust code to use the kernel's [`struct mutex`].

use super::{Guard, Lock, LockClassKey, LockFactory, LockIniter, ReadLock};
use crate::{bindings, str::CStr, Opaque};
use core::{cell::UnsafeCell, marker::PhantomPinned, pin::Pin};
pub use macros::RcuField;

/// Safely initialises a [`Mutex`] with the given name, generating a new lock class.
#[macro_export]
macro_rules! rcu_mutex_init {
    ($mutex:expr, $name:literal) => {
        $crate::init_with_lockdep!($mutex, $name)
    };
}

/// Exposes the kernel's [`struct mutex`]. When multiple threads attempt to lock the same mutex,
/// only one at a time is allowed to progress, the others will block (sleep) until the mutex is
/// unlocked, at which point another thread will be allowed to wake up and make progress.
///
/// A [`Mutex`] must first be initialised with a call to [`Mutex::init_lock`] before it can be
/// used. The [`mutex_init`] macro is provided to automatically assign a new lock class to a mutex
/// instance.
///
/// Since it may block, [`Mutex`] needs to be used with care in atomic contexts.
///
/// [`struct mutex`]: ../../../include/linux/mutex.h
pub struct RcuMutex<T: ?Sized> {
    /// The kernel `struct mutex` object.
    mutex: Opaque<bindings::mutex>,

    /// A mutex needs to be pinned because it contains a [`struct list_head`] that is
    /// self-referential, so it cannot be safely moved once it is initialised.
    _pin: PhantomPinned,

    /// The data protected by the mutex.
    data: UnsafeCell<T>,
}

// SAFETY: `Mutex` can be transferred across thread boundaries iff the data it protects can.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: ?Sized + Send> Send for RcuMutex<T> {}

// SAFETY: `Mutex` serialises the interior mutability it provides, so it is `Sync` as long as the
// data it protects is `Send`.
unsafe impl<T: ?Sized + Send> Sync for RcuMutex<T> {}

impl<T> RcuMutex<T> {
    /// Constructs a new mutex.
    ///
    /// # Safety
    ///
    /// The caller must call [`Mutex::init_lock`] before using the mutex.
    pub const unsafe fn new(t: T) -> Self {
        Self {
            mutex: Opaque::uninit(),
            data: UnsafeCell::new(t),
            _pin: PhantomPinned,
        }
    }
}

impl<T: ?Sized> RcuMutex<T> {
    /// Locks the mutex and gives the caller access to the data protected by it. Only one thread at
    /// a time is allowed to access the protected data.
    pub fn lock(&self) -> Guard<'_, Self, ReadLock> {
        let ctx = self.lock_noguard();
        // SAFETY: The mutex was just acquired.
        unsafe { Guard::new(self, ctx) }
    }
}

impl<T> LockFactory for RcuMutex<T> {
    type LockedType<U> = RcuMutex<U>;

    unsafe fn new_lock<U>(data: U) -> RcuMutex<U> {
        // SAFETY: The safety requirements of `new_lock` also require that `init_lock` be called.
        unsafe { RcuMutex::new(data) }
    }
}

impl<T> LockIniter for RcuMutex<T> {
    fn init_lock(self: Pin<&mut Self>, name: &'static CStr, key: &'static LockClassKey) {
        unsafe { bindings::__mutex_init(self.mutex.get(), name.as_char_ptr(), key.get()) };
    }
}

pub struct EmptyGuardContext;

// SAFETY: The underlying kernel `struct mutex` object ensures mutual exclusion.
unsafe impl<T: ?Sized> Lock<ReadLock> for RcuMutex<T> {
    type Inner = T;
    type GuardContext = EmptyGuardContext;

    fn lock_noguard(&self) -> EmptyGuardContext {
        // SAFETY: `mutex` points to valid memory.
        unsafe { bindings::mutex_lock(self.mutex.get()) };
        EmptyGuardContext
    }

    unsafe fn unlock(&self, _: &mut EmptyGuardContext) {
        // SAFETY: The safety requirements of the function ensure that the mutex is owned by the
        // caller.
        unsafe { bindings::mutex_unlock(self.mutex.get()) };
    }

    fn locked_data(&self) -> &UnsafeCell<T> {
        &self.data
    }
}

pub unsafe trait RcuGuardField<T>: crate::projection::Field<T> {
    type Wrapper<'a, U: ?Sized + 'a>;
}

pub unsafe trait RcuField<T>: crate::projection::Field<T> {}

impl<'a, T, F> crate::projection::Projectable<T, F> for &'a RcuMutex<T>
where
    F: RcuField<T>,
    F::Type: 'a,
{
    type Target = &'a F::Type;

    unsafe fn project(self) -> Self::Target {
        unsafe { &*F::map(self.data.get()) }
    }
}


impl<'a, T, F> crate::projection::Projectable<T, F> for &'a mut Guard<'_, RcuMutex<T>, ReadLock>
where
    F: RcuGuardField<T>,
    F::Type: 'a,
{
    type Target = F::Wrapper<'a, F::Type>;

    unsafe fn project(self) -> Self::Target {
        let ptr = unsafe { F::map(self.lock.data.get()) };
        unsafe { core::mem::transmute_copy(&ptr) }
    }
}

